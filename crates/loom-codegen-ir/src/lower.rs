use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::fmt;

use loom_core::Span;
use loom_mir::{
    self as mir, BinaryOp, CallArgument, CallTarget, ExprId, ExprKind, FunctionId, LocalId,
    StatementKind, Type, UnaryOp,
};

use crate::aggregate_plan::{
    AggregatePlanner, AggregateRegistrationError, closed_record_fields, is_direct_scalar,
};
use crate::instance_closure::{
    InstanceClosureError, InstanceClosureOutcome, InstanceClosureUnsupportedKind,
    InstanceSubstitution, InstantiationError, plan_instance_closure,
};
use crate::match_plan::{MatchNode, MatchPlan, plan_match};
use crate::place_plan::{PlaceBudget, PlacePlan, PlaceUse};
use crate::text_plan::TextLiteralBudget;
use crate::{
    ArtifactRootRequest, BlockId, BlockTarget, BoolPredicate, BuildError, BuildErrorCode,
    CheckedArtifact, CheckedIntBinaryOp, Constant, ContractFaultMetadata, Effects, FloatBinaryOp,
    FloatPredicate, FunctionBuilder, InstanceId, InstanceKey, InstancePlan, InstructionKind,
    IntPredicate, Origin, ProgramBuilder, ResourceKind, ResultTarget, Signature, SourceRoots,
    SumCase, TargetLayout, Terminator, TerminatorKind, TestOutcomePlan, UnwindTarget, ValueId,
    ValueTypeId, analyze_source_reachability,
};

const DIRECT_CLEANUP_MAX_ACTIVE_ACTIONS: usize = 1_024;
const DIRECT_CLEANUP_MAX_EXPANSIONS: usize = 65_536;

/// Source-level roots selected for one attempted LCIR artifact.
///
/// Run requests intentionally name an export instead of carrying an unchecked
/// MIR identity. Test requests use the checked program's ordered test table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceArtifactRequest {
    Run { entry: String },
    Tests,
}

/// The atomic result of LCIR route selection for one complete artifact.
#[derive(Clone, Debug, Eq, PartialEq)]
#[expect(
    clippy::large_enum_variant,
    reason = "the successful compiler path returns an owned checked artifact without a second allocation"
)]
pub enum LoweringOutcome {
    Complete(CheckedArtifact),
    Unsupported(SupportReport),
}

/// Stable invalid-root categories. Invalid roots are errors, never fallback.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum InvalidRootCode {
    UnknownEntry,
    InvalidFunction,
    DuplicateTest,
    RootSignature,
}

impl InvalidRootCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnknownEntry => "LcirLoweringUnknownEntry",
            Self::InvalidFunction => "LcirLoweringInvalidRootFunction",
            Self::DuplicateTest => "LcirLoweringDuplicateTestRoot",
            Self::RootSignature => "LcirLoweringRootSignature",
        }
    }
}

impl fmt::Display for InvalidRootCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Stable resource-limit categories. Exhaustion is an error, never fallback.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ResourceLimitCode {
    ProgramTooLarge,
}

impl ResourceLimitCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProgramTooLarge => "LcirLoweringProgramTooLarge",
        }
    }
}

impl fmt::Display for ResourceLimitCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Stable compiler-defect categories. Defects are errors, never fallback.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum LoweringDefectCode {
    SourceGraph,
    InconsistentPlan,
    Builder,
    GeneratedProgram,
    GeneratedArtifact,
}

impl LoweringDefectCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SourceGraph => "LcirLoweringSourceGraphDefect",
            Self::InconsistentPlan => "LcirLoweringInconsistentPlan",
            Self::Builder => "LcirLoweringBuilderDefect",
            Self::GeneratedProgram => "LcirLoweringGeneratedProgramDefect",
            Self::GeneratedArtifact => "LcirLoweringGeneratedArtifactDefect",
        }
    }
}

impl fmt::Display for LoweringDefectCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Stable top-level code retaining the error class and its specific reason.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum LoweringErrorCode {
    InvalidRoot(InvalidRootCode),
    ResourceLimit(ResourceLimitCode),
    Defect(LoweringDefectCode),
}

impl fmt::Display for LoweringErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRoot(code) => code.fmt(formatter),
            Self::ResourceLimit(code) => code.fmt(formatter),
            Self::Defect(code) => code.fmt(formatter),
        }
    }
}

/// Failure to select or construct an LCIR artifact.
///
/// Valid-but-unimplemented MIR is deliberately absent from this type and is
/// represented only by [`LoweringOutcome::Unsupported`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LoweringError {
    InvalidRoot {
        code: InvalidRootCode,
        message: String,
    },
    ResourceLimit {
        code: ResourceLimitCode,
        message: String,
    },
    Defect {
        code: LoweringDefectCode,
        message: String,
    },
}

impl LoweringError {
    #[must_use]
    pub const fn code(&self) -> LoweringErrorCode {
        match self {
            Self::InvalidRoot { code, .. } => LoweringErrorCode::InvalidRoot(*code),
            Self::ResourceLimit { code, .. } => LoweringErrorCode::ResourceLimit(*code),
            Self::Defect { code, .. } => LoweringErrorCode::Defect(*code),
        }
    }

    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::InvalidRoot { message, .. }
            | Self::ResourceLimit { message, .. }
            | Self::Defect { message, .. } => message,
        }
    }

    fn invalid_root(code: InvalidRootCode, message: impl Into<String>) -> Self {
        Self::InvalidRoot {
            code,
            message: message.into(),
        }
    }

    fn defect(code: LoweringDefectCode, message: impl Into<String>) -> Self {
        Self::Defect {
            code,
            message: message.into(),
        }
    }

    fn from_build_error(error: &BuildError) -> Self {
        if error.code() == BuildErrorCode::ProgramTooLarge {
            Self::ResourceLimit {
                code: ResourceLimitCode::ProgramTooLarge,
                message: error.to_string(),
            }
        } else {
            Self::defect(LoweringDefectCode::Builder, error.to_string())
        }
    }
}

impl From<BuildError> for LoweringError {
    fn from(error: BuildError) -> Self {
        Self::from_build_error(&error)
    }
}

impl fmt::Display for LoweringError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code(), self.message())
    }
}

impl Error for LoweringError {}

/// A stable coverage category for checked MIR not implemented by this slice.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum UnsupportedFeature {
    GenericFunction,
    AsyncFunction,
    WitnessParameters,
    MutableParameter,
    MutableReceiver,
    Contracts,
    SignatureType,
    ExpressionType,
    ProjectedPlace,
    ListValue,
    PatternMatch,
    NominalValue,
    RefinedValue,
    SerializedProofRecheck,
    DynamicDispatch,
    BuiltinCall,
    GenericCall,
    GenericInstanceBudget,
    NonRegularGenericRecursion,
    UnresolvedGenericInstantiation,
    WitnessArguments,
    InOutArgument,
    View,
    Suspension,
    TaskOperation,
    TextConstant,
}

impl UnsupportedFeature {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GenericFunction => "GenericFunction",
            Self::AsyncFunction => "AsyncFunction",
            Self::WitnessParameters => "WitnessParameters",
            Self::MutableParameter => "MutableParameter",
            Self::MutableReceiver => "MutableReceiver",
            Self::Contracts => "Contracts",
            Self::SignatureType => "SignatureType",
            Self::ExpressionType => "ExpressionType",
            Self::ProjectedPlace => "ProjectedPlace",
            Self::ListValue => "ListValue",
            Self::PatternMatch => "PatternMatch",
            Self::NominalValue => "NominalValue",
            Self::RefinedValue => "RefinedValue",
            Self::SerializedProofRecheck => "SerializedProofRecheck",
            Self::DynamicDispatch => "DynamicDispatch",
            Self::BuiltinCall => "BuiltinCall",
            Self::GenericCall => "GenericCall",
            Self::GenericInstanceBudget => "GenericInstanceBudget",
            Self::NonRegularGenericRecursion => "NonRegularGenericRecursion",
            Self::UnresolvedGenericInstantiation => "UnresolvedGenericInstantiation",
            Self::WitnessArguments => "WitnessArguments",
            Self::InOutArgument => "InOutArgument",
            Self::View => "View",
            Self::Suspension => "Suspension",
            Self::TaskOperation => "TaskOperation",
            Self::TextConstant => "TextConstant",
        }
    }
}

impl fmt::Display for UnsupportedFeature {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One deterministic source location outside current LCIR coverage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnsupportedItem {
    feature: UnsupportedFeature,
    function: FunctionId,
    expression: Option<ExprId>,
    span: Span,
    path: String,
}

impl UnsupportedItem {
    #[must_use]
    pub const fn feature(&self) -> UnsupportedFeature {
        self.feature
    }

    #[must_use]
    pub const fn function(&self) -> FunctionId {
        self.function
    }

    #[must_use]
    pub const fn expression(&self) -> Option<ExprId> {
        self.expression
    }

    #[must_use]
    pub const fn span(&self) -> Span {
        self.span
    }

    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }
}

/// Complete deterministic coverage report for the reachable source graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupportReport {
    items: Vec<UnsupportedItem>,
}

impl SupportReport {
    #[must_use]
    pub fn items(&self) -> &[UnsupportedItem] {
        &self.items
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

impl fmt::Display for SupportReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "reachable MIR uses {} unsupported LCIR feature site(s)",
            self.items.len()
        )
    }
}

struct SelectedRoots {
    source: SourceRoots,
    ordered: Vec<FunctionId>,
    tests: bool,
    test_outcomes: Vec<TestOutcomePlan>,
}

/// Classifies and, only when complete, lowers one whole checked-MIR artifact.
///
/// Classification always closes the source graph before allocating LCIR. An
/// unsupported site therefore selects fallback for the entire run/test
/// artifact. Invalid roots, graph defects, resource exhaustion, and invalid
/// compiler-generated LCIR are returned as errors and can never select a
/// fallback route.
///
/// # Errors
///
/// Returns a structured error for an invalid request or a compiler/resource
/// failure while constructing the complete artifact.
#[allow(clippy::too_many_lines)]
pub fn lower_typed_artifact(
    mir: &mir::CheckedProgram,
    request: &SourceArtifactRequest,
    target: TargetLayout,
) -> Result<LoweringOutcome, LoweringError> {
    let selected = select_roots(mir, request)?;
    let _graph = analyze_source_reachability(mir, &selected.source).map_err(|error| {
        LoweringError::defect(
            LoweringDefectCode::SourceGraph,
            format!("checked-MIR reachability failed: {error}"),
        )
    })?;
    let closure = match plan_instance_closure(mir.as_program(), &selected.ordered)
        .map_err(instance_closure_error)?
    {
        InstanceClosureOutcome::Complete(closure) => closure,
        InstanceClosureOutcome::Unsupported(issue) => {
            let feature = match issue.kind {
                InstanceClosureUnsupportedKind::InstanceBudget => {
                    UnsupportedFeature::GenericInstanceBudget
                }
                InstanceClosureUnsupportedKind::NonRegularRecursion => {
                    UnsupportedFeature::NonRegularGenericRecursion
                }
                InstanceClosureUnsupportedKind::Instantiation => {
                    UnsupportedFeature::UnresolvedGenericInstantiation
                }
            };
            return Ok(LoweringOutcome::Unsupported(SupportReport {
                items: vec![UnsupportedItem {
                    feature,
                    function: issue.function,
                    expression: issue.expression,
                    span: issue.span,
                    path: issue.path,
                }],
            }));
        }
    };
    let mut classifier = Classifier::new(mir.as_program(), target);
    for key in closure.entries() {
        let source = mir.function(key.source()).ok_or_else(|| {
            LoweringError::defect(
                LoweringDefectCode::SourceGraph,
                format!("reachable function #{} does not exist", key.source().0),
            )
        })?;
        classifier.classify_function(source, key);
    }
    if !classifier.items.is_empty() {
        return Ok(LoweringOutcome::Unsupported(SupportReport {
            items: classifier.items,
        }));
    }
    let Classifier {
        aggregates,
        match_plans,
        immortal_text,
        managed_text,
        ..
    } = classifier;
    // Product-contained Text uses the managed-capable pointer provenance mode
    // even when every current value is a compiler literal. This keeps the
    // product representation exact without expanding the separate immortal
    // provenance proof through aggregate construction, projection, and phi
    // flow. A literal pointer remains a valid typed managed-root cell value.
    let managed_text = managed_text || aggregates.uses_text_product_leaf();
    let aggregate_plan = aggregates.finish();
    let summaries = closure
        .entries()
        .iter()
        .map(|key| {
            let function = mir.function(key.source()).ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::SourceGraph,
                    format!("reachable function #{} disappeared", key.source().0),
                )
            })?;
            let calls = closure.calls(key).ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    format!("instance closure omitted call edges for {key}"),
                )
            })?;
            Ok(summarize_effects(function, key, calls))
        })
        .collect::<Result<Vec<_>, LoweringError>>()?;
    let effects = solve_effects(summaries)?;
    let mut builder = ProgramBuilder::new(target);
    if managed_text {
        builder
            .add_managed_text_type()
            .map_err(LoweringError::from)?;
    } else if immortal_text {
        builder
            .add_immortal_text_type()
            .map_err(LoweringError::from)?;
    }
    aggregate_plan
        .register(&mut builder)
        .map_err(|error| match error {
            AggregateRegistrationError::Build(error) => LoweringError::from(error),
            AggregateRegistrationError::Inconsistent(message) => {
                LoweringError::defect(LoweringDefectCode::InconsistentPlan, message)
            }
        })?;
    for (index, planned) in effects.entries().iter().enumerate() {
        let function_id = planned.key.source();
        let function = mir.function(function_id).ok_or_else(|| {
            LoweringError::defect(
                LoweringDefectCode::SourceGraph,
                format!("reachable function #{} disappeared", function_id.0),
            )
        })?;
        let substitution = InstanceSubstitution::new(mir.as_program(), &planned.key);
        let params = function
            .params
            .iter()
            .map(|parameter| {
                let ty = substitution
                    .instantiate_type(&parameter.ty)
                    .map_err(|error| instantiation_defect(function.id, None, error))?;
                required_type(&builder, &ty)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let result_ty = substitution
            .instantiate_type(&function.return_ty)
            .map_err(|error| instantiation_defect(function.id, None, error))?;
        let result = required_type(&builder, &result_ty)?;
        let signature = if function.receiver == Some(mir::Receiver::Mutable) {
            Signature::with_inout_params(params, effect_result(result), [0_u32])
        } else {
            Signature::new(params, effect_result(result))
        };
        let instance = builder
            .declare_instance(
                planned.key.clone(),
                Origin {
                    source_function: function.id,
                    expression: None,
                    span: function.span,
                },
                &function.name,
                signature,
                planned.effects,
            )
            .map_err(LoweringError::from)?;
        if instance.index() != index {
            return Err(LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                format!(
                    "effect-plan entry {index} was assigned unexpected LCIR instance {instance}"
                ),
            ));
        }
    }
    let instances = InstanceLookup::new(builder.instances())?;
    let instance_effects = effects
        .entries()
        .iter()
        .map(|entry| entry.effects)
        .collect::<Vec<_>>();
    for planned in effects.entries() {
        let function_id = planned.key.source();
        let source = mir.function(function_id).ok_or_else(|| {
            LoweringError::defect(
                LoweringDefectCode::SourceGraph,
                format!("reachable function #{} disappeared", function_id.0),
            )
        })?;
        let instance = instances.get(&planned.key).ok_or_else(|| {
            LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                format!("function #{} has no LCIR declaration", function_id.0),
            )
        })?;
        let function_builder = builder.function(instance).map_err(LoweringError::from)?;
        FunctionLowerer::new(
            mir.as_program(),
            source,
            &planned.key,
            function_builder,
            &instances,
            &instance_effects,
            &match_plans,
        )
        .lower()?;
    }
    let checked = builder.finish_checked().map_err(|errors| {
        LoweringError::defect(
            LoweringDefectCode::GeneratedProgram,
            format!("compiler-generated LCIR failed validation: {errors}"),
        )
    })?;
    let lowered_roots = selected
        .ordered
        .iter()
        .map(|source| {
            let key = InstanceKey::monomorphic(*source);
            instances.get(&key).ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    format!("root function #{} has no LCIR instance", source.0),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let roots = if selected.tests {
        ArtifactRootRequest::planned_tests(
            lowered_roots
                .into_iter()
                .zip(selected.test_outcomes.iter().copied()),
        )
    } else {
        ArtifactRootRequest::Run(lowered_roots.first().copied().ok_or_else(|| {
            LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                "run artifact plan contains no lowered root",
            )
        })?)
    };
    let artifact = checked.into_artifact(roots).map_err(|errors| {
        LoweringError::defect(
            LoweringDefectCode::GeneratedArtifact,
            format!("compiler-generated LCIR roots failed validation: {errors}"),
        )
    })?;
    Ok(LoweringOutcome::Complete(artifact))
}

fn instance_closure_error(error: InstanceClosureError) -> LoweringError {
    let message = match error {
        InstanceClosureError::MissingFunction(function) => {
            format!(
                "instance closure references missing function #{}",
                function.0
            )
        }
        InstanceClosureError::InvalidInstanceArity {
            function,
            expected_types,
            actual_types,
            expected_witnesses,
            actual_witnesses,
        } => format!(
            "instance of function #{} has {actual_types}/{actual_witnesses} type/witness argument(s), expected {expected_types}/{expected_witnesses}",
            function.0
        ),
        InstanceClosureError::InvalidCheckedInstantiation {
            function,
            expression,
        } => format!(
            "checked MIR function #{} expression #{} has inconsistent concrete type or witness metadata",
            function.0, expression.0
        ),
    };
    LoweringError::defect(LoweringDefectCode::InconsistentPlan, message)
}

fn instantiation_defect(
    function: FunctionId,
    expression: Option<ExprId>,
    error: InstantiationError,
) -> LoweringError {
    LoweringError::defect(
        LoweringDefectCode::InconsistentPlan,
        format!(
            "planned instance of function #{}{} failed bounded type substitution: {error:?}",
            function.0,
            expression.map_or_else(String::new, |expression| format!(
                " expression #{}",
                expression.0
            ))
        ),
    )
}

// Keeps signature construction visibly separate from semantic type lookup;
// later representation expansion may attach ABI-specific result planning.
const fn effect_result(result: ValueTypeId) -> ValueTypeId {
    result
}

fn required_type(builder: &ProgramBuilder, ty: &Type) -> Result<ValueTypeId, LoweringError> {
    builder.type_id(ty).ok_or_else(|| {
        LoweringError::defect(
            LoweringDefectCode::InconsistentPlan,
            format!("classified direct type {ty:?} has no LCIR representation"),
        )
    })
}

fn select_roots(
    checked: &mir::CheckedProgram,
    request: &SourceArtifactRequest,
) -> Result<SelectedRoots, LoweringError> {
    let program = checked.as_program();
    let (source, ordered, tests) = match request {
        SourceArtifactRequest::Run { entry } => {
            let source = SourceRoots::for_entry(checked, entry).ok_or_else(|| {
                LoweringError::invalid_root(
                    InvalidRootCode::UnknownEntry,
                    format!("run entry `{entry}` is not exported"),
                )
            })?;
            let root = source.functions().iter().copied().next().ok_or_else(|| {
                LoweringError::invalid_root(
                    InvalidRootCode::InvalidFunction,
                    format!("run entry `{entry}` selected no function"),
                )
            })?;
            (source, vec![root], false)
        }
        SourceArtifactRequest::Tests => {
            let mut seen = BTreeSet::new();
            for (index, root) in program.tests.iter().copied().enumerate() {
                if !seen.insert(root) {
                    return Err(LoweringError::invalid_root(
                        InvalidRootCode::DuplicateTest,
                        format!("test root #{} at index {index} is duplicated", root.0),
                    ));
                }
            }
            (SourceRoots::for_tests(checked), program.tests.clone(), true)
        }
    };

    for root in &ordered {
        let function = program.function(*root).ok_or_else(|| {
            LoweringError::invalid_root(
                InvalidRootCode::InvalidFunction,
                format!("artifact root function #{} does not exist", root.0),
            )
        })?;
        let hidden_inputs = function.type_parameters != 0
            || !function.witness_params.is_empty()
            || function.witness_prefix_count != 0
            || function.receiver.is_some();
        let invalid_signature = if tests {
            hidden_inputs
                || !function.params.is_empty()
                || !is_valid_test_return(program, &function.return_ty)
        } else {
            hidden_inputs || !function.params.is_empty() || function.return_ty != Type::Unit
        };
        if invalid_signature {
            let expected = if tests {
                "have no inputs and return Unit or Result[Unit, E]"
            } else {
                "have signature () -> Unit"
            };
            return Err(LoweringError::invalid_root(
                InvalidRootCode::RootSignature,
                format!("artifact root `{}` must {expected}", function.name),
            ));
        }
    }
    let test_outcomes = if tests {
        ordered
            .iter()
            .map(|root| {
                let function = program.function(*root).ok_or_else(|| {
                    LoweringError::defect(
                        LoweringDefectCode::SourceGraph,
                        format!("test root function #{} disappeared", root.0),
                    )
                })?;
                test_outcome_plan(program, &function.return_ty).ok_or_else(|| {
                    LoweringError::defect(
                        LoweringDefectCode::InconsistentPlan,
                        format!(
                            "validated test root `{}` has no outcome plan",
                            function.name
                        ),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };
    Ok(SelectedRoots {
        source,
        ordered,
        tests,
        test_outcomes,
    })
}

fn is_valid_test_return(program: &mir::Program, ty: &Type) -> bool {
    test_outcome_plan(program, ty).is_some()
}

fn test_outcome_plan(program: &mir::Program, ty: &Type) -> Option<TestOutcomePlan> {
    if *ty == Type::Unit {
        return Some(TestOutcomePlan::Unit);
    }
    let result = program.prelude.result?;
    matches!(
        ty,
        Type::Nominal(type_id, arguments)
            if *type_id == result
                && arguments.len() == 2
                && arguments.first() == Some(&Type::Unit)
    )
    .then_some(TestOutcomePlan::Result {
        success_variant: 0,
        failure_variant: 1,
    })
}

const fn is_scalar_type(ty: &Type) -> bool {
    is_direct_scalar(ty)
}

struct Classifier<'program> {
    program: &'program mir::Program,
    target: TargetLayout,
    items: Vec<UnsupportedItem>,
    aggregates: AggregatePlanner<'program>,
    match_plans: BTreeMap<String, BTreeMap<ExprId, MatchPlan>>,
    places: PlaceBudget,
    text_literals: TextLiteralBudget,
    immortal_text: bool,
    managed_text: bool,
}

#[derive(Clone, Copy)]
struct PlaceSite {
    expression: Option<ExprId>,
    span: Span,
}

impl PlaceSite {
    const fn statement(span: Span) -> Self {
        Self {
            expression: None,
            span,
        }
    }

    const fn expression(expression: &mir::Expr) -> Self {
        Self {
            expression: Some(expression.id),
            span: expression.span,
        }
    }
}

impl<'program> Classifier<'program> {
    fn new(program: &'program mir::Program, target: TargetLayout) -> Self {
        Self {
            program,
            target,
            items: Vec::new(),
            aggregates: AggregatePlanner::new(program, target.pointer_bits() == 64),
            match_plans: BTreeMap::new(),
            places: PlaceBudget::default(),
            text_literals: TextLiteralBudget::default(),
            immortal_text: false,
            managed_text: false,
        }
    }

    fn supported_value_type(&mut self, ty: &Type) -> bool {
        if ty == &Type::Text {
            if self.target.pointer_bits() != 64 {
                return false;
            }
            self.immortal_text = true;
            return true;
        }
        self.aggregates.supports_value_type(ty)
    }

    fn supported_record_type(&mut self, ty: &Type) -> bool {
        closed_record_fields(self.program, ty).is_some() && self.aggregates.supports_value_type(ty)
    }

    fn supported_expression_type(&mut self, ty: &Type) -> bool {
        matches!(ty, Type::Never) || self.supported_value_type(ty)
    }

    fn local_type(function: &mir::Function, local: LocalId) -> Option<&Type> {
        function
            .params
            .iter()
            .chain(&function.locals)
            .find(|candidate| candidate.id == local)
            .map(|candidate| &candidate.ty)
    }

    fn instantiated_type(
        &mut self,
        function: &mir::Function,
        key: &InstanceKey,
        expression: Option<&mir::Expr>,
        ty: &Type,
        span: Span,
        path: &str,
    ) -> Option<Type> {
        let Ok(ty) = InstanceSubstitution::new(self.program, key).instantiate_type(ty) else {
            self.item(
                UnsupportedFeature::UnresolvedGenericInstantiation,
                function.id,
                expression.map(|expression| expression.id),
                span,
                path.to_owned(),
            );
            return None;
        };
        Some(ty)
    }

    fn supported_projected_place(
        &mut self,
        function: &mir::Function,
        key: &InstanceKey,
        place: &mir::Place,
        usage: PlaceUse,
        span: Span,
        path: &str,
    ) -> Option<Type> {
        if !self.places.admit(usage, place.projection.len()) {
            return None;
        }
        let base = Self::local_type(function, place.local)?;
        let mut ty = self.instantiated_type(function, key, None, base, span, path)?;
        if !self.supported_value_type(&ty) {
            return None;
        }
        for field in &place.projection {
            let fields = closed_record_fields(self.program, &ty)?;
            let next = usize::try_from(*field)
                .ok()
                .and_then(|index| fields.get(index))
                .map(|field| field.ty.clone())?;
            ty = next;
        }
        self.supported_value_type(&ty).then_some(ty)
    }

    fn classify_function(&mut self, function: &mir::Function, key: &InstanceKey) {
        let base = format!("function[{}]", function.id.0);
        if function.is_async || !function.suspension_points.is_empty() {
            self.function_item(UnsupportedFeature::AsyncFunction, function, &base);
        }
        let mutable_pod_receiver = function.receiver == Some(mir::Receiver::Mutable)
            && function
                .params
                .first()
                .and_then(|parameter| {
                    self.instantiated_type(
                        function,
                        key,
                        None,
                        &parameter.ty,
                        parameter.span,
                        &format!("{base}.params[0]"),
                    )
                })
                .is_some_and(|ty| self.supported_record_type(&ty));
        if function.receiver == Some(mir::Receiver::Mutable) && !mutable_pod_receiver {
            self.function_item(UnsupportedFeature::MutableReceiver, function, &base);
        }
        if function.call_plan.receiver_invariant.is_some()
            || !function.call_plan.requires.is_empty()
            || !function.call_plan.ensures.is_empty()
        {
            self.function_item(
                UnsupportedFeature::Contracts,
                function,
                &format!("{base}.call_plan"),
            );
        }
        for (index, parameter) in function.params.iter().enumerate() {
            let path = format!("{base}.params[{index}]");
            let supported_inout_receiver = index == 0
                && function.receiver == Some(mir::Receiver::Mutable)
                && InstanceSubstitution::new(self.program, key)
                    .instantiate_type(&parameter.ty)
                    .is_ok_and(|ty| self.supported_record_type(&ty));
            if parameter.mutable && !supported_inout_receiver {
                self.item(
                    UnsupportedFeature::MutableParameter,
                    function.id,
                    None,
                    parameter.span,
                    path.clone(),
                );
            }
            let supported = self
                .instantiated_type(function, key, None, &parameter.ty, parameter.span, &path)
                .is_some_and(|ty| self.supported_value_type(&ty));
            if !supported {
                self.item(
                    UnsupportedFeature::SignatureType,
                    function.id,
                    None,
                    parameter.span,
                    path,
                );
            }
        }
        let return_path = format!("{base}.return_ty");
        let supported_return = self
            .instantiated_type(
                function,
                key,
                None,
                &function.return_ty,
                function.span,
                &return_path,
            )
            .is_some_and(|ty| self.supported_value_type(&ty));
        if !supported_return {
            self.item(
                UnsupportedFeature::SignatureType,
                function.id,
                None,
                function.span,
                return_path,
            );
        }
        // Function-local declarations include values from syntactically dead
        // regions. Reachable expressions below carry their checked types, so
        // classifying uses rather than the whole declaration table keeps DCE
        // exact while still rejecting every executable unsupported value.
        self.visit_block(function, key, &function.body, &format!("{base}.body"));
    }

    fn function_item(&mut self, feature: UnsupportedFeature, function: &mir::Function, path: &str) {
        self.item(feature, function.id, None, function.span, path.to_owned());
    }

    fn item(
        &mut self,
        feature: UnsupportedFeature,
        function: FunctionId,
        expression: Option<ExprId>,
        span: Span,
        path: String,
    ) {
        self.items.push(UnsupportedItem {
            feature,
            function,
            expression,
            span,
            path,
        });
    }

    fn expression_item(
        &mut self,
        feature: UnsupportedFeature,
        function: &mir::Function,
        expression: &mir::Expr,
        path: &str,
    ) {
        self.item(
            feature,
            function.id,
            Some(expression.id),
            expression.span,
            path.to_owned(),
        );
    }

    fn projected_place(
        &mut self,
        function: &mir::Function,
        key: &InstanceKey,
        place: &mir::Place,
        usage: PlaceUse,
        site: PlaceSite,
        path: &str,
    ) -> Option<Type> {
        let projected =
            self.supported_projected_place(function, key, place, usage, site.span, path);
        if projected.is_none() {
            self.item(
                UnsupportedFeature::ProjectedPlace,
                function.id,
                site.expression,
                site.span,
                path.to_owned(),
            );
        }
        projected
    }

    fn visit_block(
        &mut self,
        function: &mir::Function,
        key: &InstanceKey,
        block: &mir::Block,
        path: &str,
    ) -> bool {
        for (index, statement) in block.statements.iter().enumerate() {
            let statement_path = format!("{path}.statements[{index}]");
            if !self.visit_statement(function, key, statement, &statement_path) {
                return false;
            }
        }
        if let Some(tail) = block.tail.as_deref() {
            self.visit_expr(function, key, tail, &format!("{path}.tail"))
        } else {
            true
        }
    }

    #[allow(clippy::too_many_lines)]
    fn visit_statement(
        &mut self,
        function: &mir::Function,
        key: &InstanceKey,
        statement: &mir::Statement,
        path: &str,
    ) -> bool {
        match &statement.kind {
            StatementKind::Let { value, .. }
            | StatementKind::LetTuple { value, .. }
            | StatementKind::Scoped { value, .. } => {
                self.visit_expr(function, key, value, &format!("{path}.value"))
            }
            StatementKind::ForRange {
                start, end, body, ..
            } => {
                let start_continues =
                    self.visit_expr(function, key, start, &format!("{path}.start"));
                if !start_continues {
                    return false;
                }
                let end_continues = self.visit_expr(function, key, end, &format!("{path}.end"));
                if !end_continues {
                    return false;
                }
                self.visit_block(function, key, body, &format!("{path}.body"));
                true
            }
            StatementKind::Assign { place, value } => {
                if !self.visit_expr(function, key, value, &format!("{path}.value")) {
                    return false;
                }
                self.projected_place(
                    function,
                    key,
                    place,
                    PlaceUse::Write,
                    PlaceSite::statement(statement.span),
                    &format!("{path}.place"),
                );
                true
            }
            StatementKind::Assert { condition } => {
                self.visit_expr(function, key, condition, &format!("{path}.condition"))
            }
            StatementKind::Evaluate(expression) => {
                self.visit_expr(function, key, expression, &format!("{path}.value"))
            }
            StatementKind::Defer(cleanup) => {
                self.visit_block(function, key, cleanup, &format!("{path}.cleanup"));
                true
            }
            StatementKind::Return(value) => {
                if let Some(value) = value {
                    self.visit_expr(function, key, value, &format!("{path}.value"));
                }
                false
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn visit_expr(
        &mut self,
        function: &mir::Function,
        key: &InstanceKey,
        expression: &mir::Expr,
        path: &str,
    ) -> bool {
        let continues = match &expression.kind {
            ExprKind::Constant(mir::Constant::Text(value)) => {
                if self.target.pointer_bits() != 64 || !self.text_literals.admit(value.len()) {
                    self.expression_item(
                        UnsupportedFeature::TextConstant,
                        function,
                        expression,
                        path,
                    );
                } else {
                    self.immortal_text = true;
                }
                true
            }
            ExprKind::Constant(_) => true,
            ExprKind::Tuple(elements) => {
                if !self.visit_exprs(function, key, elements, &format!("{path}.elements")) {
                    return false;
                }
                expression.ty != Type::Never
            }
            ExprKind::List(elements) => {
                if !self.visit_exprs(function, key, elements, &format!("{path}.elements")) {
                    return false;
                }
                self.expression_item(UnsupportedFeature::ListValue, function, expression, path);
                expression.ty != Type::Never
            }
            ExprKind::Copy(place) | ExprKind::Move(place) => {
                let usage = if matches!(expression.kind, ExprKind::Move(_)) {
                    PlaceUse::Move
                } else {
                    PlaceUse::Read
                };
                self.projected_place(
                    function,
                    key,
                    place,
                    usage,
                    PlaceSite::expression(expression),
                    &format!("{path}.place"),
                );
                true
            }
            ExprKind::Unary(_, operand) => {
                let continues = self.visit_expr(function, key, operand, &format!("{path}.operand"));
                let scalar = self
                    .instantiated_type(
                        function,
                        key,
                        Some(operand),
                        &operand.ty,
                        operand.span,
                        &format!("{path}.operand.ty"),
                    )
                    .is_some_and(|ty| is_scalar_type(&ty));
                if continues && !scalar {
                    self.expression_item(
                        UnsupportedFeature::NominalValue,
                        function,
                        expression,
                        path,
                    );
                }
                continues && expression.ty != Type::Never
            }
            ExprKind::Binary(operator, left, right) => {
                if self.visit_expr(function, key, left, &format!("{path}.left")) {
                    let right_continues =
                        self.visit_expr(function, key, right, &format!("{path}.right"));
                    let scalar = self
                        .instantiated_type(
                            function,
                            key,
                            Some(left),
                            &left.ty,
                            left.span,
                            &format!("{path}.left.ty"),
                        )
                        .is_some_and(|ty| {
                            is_scalar_type(&ty)
                                || (ty == Type::Text
                                    && matches!(operator, BinaryOp::Equal | BinaryOp::NotEqual))
                        });
                    if right_continues && !scalar {
                        self.expression_item(
                            UnsupportedFeature::NominalValue,
                            function,
                            expression,
                            path,
                        );
                    }
                    right_continues || matches!(operator, BinaryOp::And | BinaryOp::Or)
                } else {
                    false
                }
            }
            ExprKind::Block(block) => {
                self.visit_block(function, key, block, &format!("{path}.block"))
                    && expression.ty != Type::Never
            }
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                if !self.visit_expr(function, key, condition, &format!("{path}.condition")) {
                    return false;
                }
                let then_continues =
                    self.visit_block(function, key, then_branch, &format!("{path}.then"));
                let else_continues =
                    self.visit_block(function, key, else_branch, &format!("{path}.else"));
                then_continues || else_continues
            }
            ExprKind::Match { scrutinee, arms } => {
                if !self.visit_expr(function, key, scrutinee, &format!("{path}.scrutinee")) {
                    return false;
                }
                let mut continues = false;
                for (index, arm) in arms.iter().enumerate() {
                    continues |= self.visit_expr(
                        function,
                        key,
                        &arm.value,
                        &format!("{path}.arms[{index}].value"),
                    );
                }
                let scrutinee_ty = self.instantiated_type(
                    function,
                    key,
                    Some(scrutinee),
                    &scrutinee.ty,
                    scrutinee.span,
                    &format!("{path}.scrutinee.ty"),
                );
                if scrutinee_ty
                    .as_ref()
                    .is_some_and(|ty| self.supported_value_type(ty))
                {
                    if let Some(plan) = plan_match(
                        self.program,
                        scrutinee_ty.as_ref().expect("checked Some"),
                        arms,
                    ) {
                        if self
                            .match_plans
                            .entry(key.canonical_identity())
                            .or_default()
                            .insert(expression.id, plan)
                            .is_some()
                        {
                            self.expression_item(
                                UnsupportedFeature::PatternMatch,
                                function,
                                expression,
                                path,
                            );
                        }
                    } else {
                        self.expression_item(
                            UnsupportedFeature::PatternMatch,
                            function,
                            expression,
                            path,
                        );
                    }
                } else {
                    self.expression_item(
                        UnsupportedFeature::PatternMatch,
                        function,
                        expression,
                        path,
                    );
                }
                continues
            }
            ExprKind::Record {
                ty,
                type_arguments,
                fields,
                construction,
            } => {
                if !self.visit_exprs(function, key, fields, &format!("{path}.fields")) {
                    return false;
                }
                if *construction == mir::ConstructionMode::Recheck {
                    self.expression_item(
                        UnsupportedFeature::SerializedProofRecheck,
                        function,
                        expression,
                        path,
                    );
                    return expression.ty != Type::Never;
                }
                let expression_ty = self.instantiated_type(
                    function,
                    key,
                    Some(expression),
                    &expression.ty,
                    expression.span,
                    &format!("{path}.ty"),
                );
                let instantiated_arguments =
                    InstanceSubstitution::new(self.program, key).instantiate_types(type_arguments);
                let direct_product = instantiated_arguments
                    .is_ok_and(|arguments| arguments.is_empty())
                    && expression_ty.as_ref() == Some(&Type::Nominal(*ty, Vec::new()))
                    && expression_ty
                        .as_ref()
                        .is_some_and(|ty| self.supported_value_type(ty))
                    && self.program.type_def(*ty).is_some_and(|definition| {
                        definition.type_parameters == 0
                            && matches!(
                                (&definition.kind, construction),
                                (
                                    mir::TypeDefKind::Record {
                                        invariant: None,
                                        ..
                                    },
                                    mir::ConstructionMode::Plain
                                ) | (
                                    mir::TypeDefKind::Record {
                                        invariant: Some(_),
                                        ..
                                    },
                                    mir::ConstructionMode::Proven
                                )
                            )
                    });
                if !direct_product {
                    self.expression_item(
                        UnsupportedFeature::NominalValue,
                        function,
                        expression,
                        path,
                    );
                }
                expression.ty != Type::Never
            }
            ExprKind::Variant {
                ty,
                type_arguments,
                payload,
                ..
            } => {
                if !self.visit_exprs(function, key, payload, &format!("{path}.payload")) {
                    return false;
                }
                let expression_ty = self.instantiated_type(
                    function,
                    key,
                    Some(expression),
                    &expression.ty,
                    expression.span,
                    &format!("{path}.ty"),
                );
                let semantic = InstanceSubstitution::new(self.program, key)
                    .instantiate_types(type_arguments)
                    .ok()
                    .map(|arguments| Type::Nominal(*ty, arguments));
                if expression_ty != semantic
                    || semantic
                        .as_ref()
                        .is_none_or(|semantic| !self.supported_value_type(semantic))
                {
                    self.expression_item(
                        UnsupportedFeature::NominalValue,
                        function,
                        expression,
                        path,
                    );
                }
                expression.ty != Type::Never
            }
            ExprKind::Refine {
                ty,
                value,
                construction,
            } => {
                if !self.visit_expr(function, key, value, &format!("{path}.value")) {
                    return false;
                }
                if *construction == mir::ConstructionMode::Recheck {
                    self.expression_item(
                        UnsupportedFeature::SerializedProofRecheck,
                        function,
                        expression,
                        path,
                    );
                    return expression.ty != Type::Never;
                }
                let expression_ty = self.instantiated_type(
                    function,
                    key,
                    Some(expression),
                    &expression.ty,
                    expression.span,
                    &format!("{path}.ty"),
                );
                let value_ty = self.instantiated_type(
                    function,
                    key,
                    Some(value),
                    &value.ty,
                    value.span,
                    &format!("{path}.value.ty"),
                );
                let proven = *construction == mir::ConstructionMode::Proven
                    && expression_ty.as_ref() == Some(&Type::Nominal(*ty, Vec::new()))
                    && expression_ty
                        .as_ref()
                        .is_some_and(|ty| self.supported_value_type(ty))
                    && self.program.type_def(*ty).is_some_and(|definition| {
                        definition.type_parameters == 0
                            && matches!(
                                &definition.kind,
                                mir::TypeDefKind::Refined { base, .. }
                                    if value_ty.as_ref() == Some(base)
                            )
                    });
                if !proven {
                    self.expression_item(
                        UnsupportedFeature::RefinedValue,
                        function,
                        expression,
                        path,
                    );
                }
                expression.ty != Type::Never
            }
            ExprKind::Unrefine(value) => {
                if !self.visit_expr(function, key, value, &format!("{path}.value")) {
                    return false;
                }
                let value_ty = self.instantiated_type(
                    function,
                    key,
                    Some(value),
                    &value.ty,
                    value.span,
                    &format!("{path}.value.ty"),
                );
                let expression_ty = self.instantiated_type(
                    function,
                    key,
                    Some(expression),
                    &expression.ty,
                    expression.span,
                    &format!("{path}.ty"),
                );
                let supported = match value_ty.as_ref() {
                    Some(Type::Nominal(ty, arguments)) if arguments.is_empty() => {
                        self.program.type_def(*ty).is_some_and(|definition| {
                            definition.type_parameters == 0
                                && matches!(
                                    &definition.kind,
                                    mir::TypeDefKind::Refined { base, .. }
                                        if expression_ty.as_ref() == Some(base)
                                )
                        }) && self.supported_value_type(value_ty.as_ref().expect("matched"))
                            && expression_ty
                                .as_ref()
                                .is_some_and(|ty| self.supported_value_type(ty))
                    }
                    _ => false,
                };
                if !supported {
                    self.expression_item(
                        UnsupportedFeature::RefinedValue,
                        function,
                        expression,
                        path,
                    );
                }
                expression.ty != Type::Never
            }
            ExprKind::Call {
                target,
                type_arguments,
                arguments,
                witnesses,
            } => {
                let callee_key = match target {
                    CallTarget::Direct(callee) | CallTarget::Inherent(callee) => {
                        InstanceSubstitution::new(self.program, key)
                            .call_key(*callee, type_arguments, witnesses)
                            .ok()
                    }
                    CallTarget::StaticConcept {
                        requirement,
                        witness,
                        dispatch_type,
                    } => InstanceSubstitution::new(self.program, key)
                        .static_call_key(
                            *requirement,
                            witness,
                            dispatch_type,
                            type_arguments,
                            witnesses,
                        )
                        .ok(),
                    CallTarget::Dynamic { .. } | CallTarget::Builtin(_) => None,
                };
                let mutable_receiver = callee_key.as_ref().and_then(|callee_key| {
                    self.program
                        .function(callee_key.source())
                        .filter(|callee| callee.receiver == Some(mir::Receiver::Mutable))
                        .and_then(|callee| {
                            callee.params.first().and_then(|parameter| {
                                InstanceSubstitution::new(self.program, callee_key)
                                    .instantiate_type(&parameter.ty)
                                    .ok()
                            })
                        })
                });
                for (index, argument) in arguments.iter().enumerate() {
                    match argument {
                        CallArgument::Value(value) => {
                            if !self.visit_expr(
                                function,
                                key,
                                value,
                                &format!("{path}.arguments[{index}].value"),
                            ) {
                                return false;
                            }
                        }
                        CallArgument::InOut(place) => {
                            let place_type = self.projected_place(
                                function,
                                key,
                                place,
                                PlaceUse::InOut,
                                PlaceSite::expression(expression),
                                &format!("{path}.arguments[{index}].place"),
                            );
                            let allowed = index == 0
                                && mutable_receiver.as_ref() == place_type.as_ref()
                                && place_type
                                    .as_ref()
                                    .is_some_and(|ty| self.supported_record_type(ty));
                            if !allowed {
                                self.expression_item(
                                    UnsupportedFeature::InOutArgument,
                                    function,
                                    expression,
                                    &format!("{path}.arguments[{index}]"),
                                );
                            }
                        }
                    }
                }
                let target_feature = match target {
                    CallTarget::Direct(_)
                    | CallTarget::Inherent(_)
                    | CallTarget::StaticConcept { .. } => None,
                    CallTarget::Dynamic { .. } => Some(UnsupportedFeature::DynamicDispatch),
                    CallTarget::Builtin(
                        mir::Builtin::TextLength
                        | mir::Builtin::TextContains
                        | mir::Builtin::TextConcat,
                    ) => {
                        if matches!(target, CallTarget::Builtin(mir::Builtin::TextConcat)) {
                            self.managed_text = true;
                        }
                        None
                    }
                    CallTarget::Builtin(_) => Some(UnsupportedFeature::BuiltinCall),
                };
                if let Some(feature) = target_feature {
                    self.expression_item(feature, function, expression, &format!("{path}.target"));
                }
                expression.ty != Type::Never
            }
            ExprKind::MakeView { value, .. } => {
                if !self.visit_expr(function, key, value, &format!("{path}.value")) {
                    return false;
                }
                self.expression_item(UnsupportedFeature::View, function, expression, path);
                expression.ty != Type::Never
            }
            ExprKind::ReborrowView { owner, .. } => {
                self.expression_item(UnsupportedFeature::View, function, expression, path);
                self.projected_place(
                    function,
                    key,
                    owner,
                    PlaceUse::Read,
                    PlaceSite::expression(expression),
                    &format!("{path}.owner"),
                );
                true
            }
            ExprKind::Await { task, .. } => {
                if !self.visit_expr(function, key, task, &format!("{path}.task")) {
                    return false;
                }
                self.expression_item(UnsupportedFeature::Suspension, function, expression, path);
                expression.ty != Type::Never
            }
            ExprKind::Sleep { milliseconds } => {
                if !self.visit_expr(function, key, milliseconds, &format!("{path}.milliseconds")) {
                    return false;
                }
                self.expression_item(
                    UnsupportedFeature::TaskOperation,
                    function,
                    expression,
                    path,
                );
                expression.ty != Type::Never
            }
            ExprKind::TaskJoin { arguments, .. } => {
                if !self.visit_exprs(function, key, arguments, &format!("{path}.arguments")) {
                    return false;
                }
                self.expression_item(
                    UnsupportedFeature::TaskOperation,
                    function,
                    expression,
                    path,
                );
                expression.ty != Type::Never
            }
        };
        let continues = continues && expression.ty != Type::Never;
        let supported_expression = self
            .instantiated_type(
                function,
                key,
                Some(expression),
                &expression.ty,
                expression.span,
                &format!("{path}.ty"),
            )
            .is_some_and(|ty| self.supported_expression_type(&ty));
        if continues && !supported_expression {
            self.expression_item(
                UnsupportedFeature::ExpressionType,
                function,
                expression,
                &format!("{path}.ty"),
            );
        }
        continues
    }

    fn visit_exprs(
        &mut self,
        function: &mir::Function,
        key: &InstanceKey,
        expressions: &[mir::Expr],
        path: &str,
    ) -> bool {
        for (index, expression) in expressions.iter().enumerate() {
            if !self.visit_expr(function, key, expression, &format!("{path}[{index}]")) {
                return false;
            }
        }
        true
    }
}

#[derive(Clone, Debug, Default)]
struct EffectSummary {
    local: Effects,
    calls: BTreeSet<FunctionId>,
}

impl EffectSummary {
    fn include(&mut self, effects: Effects) {
        self.local = self.local.union(effects).with_implications();
    }
}

#[derive(Clone, Debug)]
struct InstanceEffectSummary {
    key: InstanceKey,
    local: Effects,
    calls: Box<[InstanceKey]>,
}

#[cfg(test)]
impl InstanceEffectSummary {
    fn monomorphic(source: FunctionId, summary: EffectSummary) -> Self {
        Self {
            key: InstanceKey::monomorphic(source),
            local: summary.local,
            calls: summary
                .calls
                .into_iter()
                .map(InstanceKey::monomorphic)
                .collect(),
        }
    }
}

#[derive(Clone, Debug)]
struct EffectPlanEntry {
    key: InstanceKey,
    effects: Effects,
}

#[derive(Clone, Debug)]
struct EffectPlan {
    entries: Vec<EffectPlanEntry>,
}

impl EffectPlan {
    fn entries(&self) -> &[EffectPlanEntry] {
        &self.entries
    }
}

struct InstanceLookup {
    indexes: BTreeMap<String, InstanceId>,
}

impl InstanceLookup {
    fn new(plan: &InstancePlan) -> Result<Self, LoweringError> {
        let mut indexes = BTreeMap::new();
        for instance in plan.entries() {
            let identity = instance.key().canonical_identity();
            if indexes.insert(identity, instance.id()).is_some() {
                return Err(LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    format!("duplicate LCIR declaration for {}", instance.key()),
                ));
            }
        }
        Ok(Self { indexes })
    }

    fn get(&self, key: &InstanceKey) -> Option<InstanceId> {
        self.indexes.get(&key.canonical_identity()).copied()
    }
}

fn summarize_effects(
    function: &mir::Function,
    key: &InstanceKey,
    calls: &[InstanceKey],
) -> InstanceEffectSummary {
    let mut summary = EffectSummary::default();
    scan_effect_block(&function.body, &mut summary);
    InstanceEffectSummary {
        key: key.clone(),
        local: summary.local,
        calls: calls.to_vec().into_boxed_slice(),
    }
}

fn scan_effect_block(block: &mir::Block, summary: &mut EffectSummary) -> bool {
    for statement in &block.statements {
        if !scan_effect_statement(statement, summary) {
            return false;
        }
    }
    block
        .tail
        .as_deref()
        .is_none_or(|tail| scan_effect_expr(tail, summary))
}

fn scan_effect_statement(statement: &mir::Statement, summary: &mut EffectSummary) -> bool {
    match &statement.kind {
        StatementKind::Let { value, .. }
        | StatementKind::LetTuple { value, .. }
        | StatementKind::Assign { value, .. }
        | StatementKind::Evaluate(value) => scan_effect_expr(value, summary),
        StatementKind::Scoped {
            value, disposal, ..
        } => {
            let continues = scan_effect_expr(value, summary);
            if continues
                && matches!(
                    disposal,
                    mir::ScopedDisposal::FileClose | mir::ScopedDisposal::SocketClose
                )
            {
                summary.include(Effects::MAY_FAULT.union(Effects::NEEDS_RUNTIME));
            }
            continues
        }
        StatementKind::ForRange {
            start, end, body, ..
        } => {
            if !scan_effect_expr(start, summary) || !scan_effect_expr(end, summary) {
                return false;
            }
            scan_effect_block(body, summary);
            true
        }
        StatementKind::Assert { condition } => {
            let continues = scan_effect_expr(condition, summary);
            if continues {
                summary.include(Effects::MAY_FAULT);
            }
            continues
        }
        StatementKind::Defer(cleanup) => {
            scan_effect_block(cleanup, summary);
            true
        }
        StatementKind::Return(value) => {
            if let Some(value) = value {
                scan_effect_expr(value, summary);
            }
            false
        }
    }
}

#[allow(clippy::too_many_lines)]
fn scan_effect_expr(expression: &mir::Expr, summary: &mut EffectSummary) -> bool {
    match &expression.kind {
        ExprKind::Constant(_)
        | ExprKind::Copy(_)
        | ExprKind::Move(_)
        | ExprKind::ReborrowView { .. } => true,
        ExprKind::Tuple(values) | ExprKind::List(values) => scan_effect_exprs(values, summary),
        ExprKind::Unary(operator, operand) => {
            if !scan_effect_expr(operand, summary) {
                return false;
            }
            if *operator == UnaryOp::Negate && operand.ty == Type::Int {
                summary.include(Effects::MAY_FAULT);
            }
            true
        }
        ExprKind::Binary(operator, left, right) => {
            if !scan_effect_expr(left, summary) {
                return false;
            }
            let right_continues = scan_effect_expr(right, summary);
            if right_continues
                && matches!(
                    operator,
                    BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide
                )
                && left.ty == Type::Int
            {
                summary.include(Effects::MAY_FAULT);
            }
            right_continues || matches!(operator, BinaryOp::And | BinaryOp::Or)
        }
        ExprKind::Block(block) => scan_effect_block(block, summary),
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            scan_effect_expr(condition, summary)
                && (scan_effect_block(then_branch, summary)
                    | scan_effect_block(else_branch, summary))
        }
        ExprKind::Match { scrutinee, arms } => {
            if !scan_effect_expr(scrutinee, summary) {
                return false;
            }
            arms.iter().fold(false, |continues, arm| {
                scan_effect_expr(&arm.value, summary) | continues
            })
        }
        ExprKind::Record { fields, .. } => scan_effect_exprs(fields, summary),
        ExprKind::Variant { payload, .. } => scan_effect_exprs(payload, summary),
        ExprKind::Refine { value, .. } | ExprKind::Unrefine(value) => {
            scan_effect_expr(value, summary)
        }
        ExprKind::Call {
            target, arguments, ..
        } => {
            for argument in arguments {
                if let CallArgument::Value(value) = argument
                    && !scan_effect_expr(value, summary)
                {
                    return false;
                }
            }
            if let CallTarget::Direct(callee) | CallTarget::Inherent(callee) = target {
                summary.calls.insert(*callee);
            } else if matches!(target, CallTarget::Builtin(mir::Builtin::TextConcat)) {
                summary.include(Effects::MAY_COLLECT);
            }
            expression.ty != Type::Never
        }
        ExprKind::MakeView { value, .. } => scan_effect_expr(value, summary),
        ExprKind::Await { task, .. } => scan_effect_expr(task, summary),
        ExprKind::Sleep { milliseconds } => scan_effect_expr(milliseconds, summary),
        ExprKind::TaskJoin { arguments, .. } => scan_effect_exprs(arguments, summary),
    }
}

fn scan_effect_exprs(expressions: &[mir::Expr], summary: &mut EffectSummary) -> bool {
    expressions
        .iter()
        .all(|expression| scan_effect_expr(expression, summary))
}

fn continuing_mutations(block: &mir::Block) -> Option<BTreeSet<LocalId>> {
    let mut changed = BTreeSet::new();
    scan_mutation_block(block, &mut changed).then_some(changed)
}

fn scan_mutation_block(block: &mir::Block, changed: &mut BTreeSet<LocalId>) -> bool {
    for statement in &block.statements {
        if !scan_mutation_statement(statement, changed) {
            return false;
        }
    }
    block
        .tail
        .as_deref()
        .is_none_or(|tail| scan_mutation_expr(tail, changed))
}

fn scan_mutation_statement(statement: &mir::Statement, changed: &mut BTreeSet<LocalId>) -> bool {
    match &statement.kind {
        StatementKind::Let { local, value } | StatementKind::Scoped { local, value, .. } => {
            let continues = scan_mutation_expr(value, changed);
            if continues {
                changed.insert(*local);
            }
            continues
        }
        StatementKind::LetTuple { locals, value } => {
            let continues = scan_mutation_expr(value, changed);
            if continues {
                changed.extend(locals.iter().copied());
            }
            continues
        }
        StatementKind::ForRange {
            local,
            start,
            end,
            body,
        } => {
            if !scan_mutation_expr(start, changed) || !scan_mutation_expr(end, changed) {
                return false;
            }
            let entry = changed.clone();
            let mut iteration = entry.clone();
            iteration.insert(*local);
            if scan_mutation_block(body, &mut iteration) {
                changed.extend(iteration);
            }
            true
        }
        StatementKind::Assign { place, value } => {
            let continues = scan_mutation_expr(value, changed);
            if continues {
                changed.insert(place.local);
            }
            continues
        }
        StatementKind::Assert { condition } | StatementKind::Evaluate(condition) => {
            scan_mutation_expr(condition, changed)
        }
        StatementKind::Defer(_) => true,
        StatementKind::Return(value) => {
            if let Some(value) = value {
                let _ = scan_mutation_expr(value, changed);
            }
            false
        }
    }
}

#[allow(clippy::too_many_lines)]
fn scan_mutation_expr(expression: &mir::Expr, changed: &mut BTreeSet<LocalId>) -> bool {
    let continues = match &expression.kind {
        ExprKind::Constant(_) | ExprKind::Copy(_) | ExprKind::ReborrowView { .. } => true,
        ExprKind::Move(place) => {
            changed.insert(place.local);
            true
        }
        ExprKind::Tuple(values) | ExprKind::List(values) => values
            .iter()
            .all(|value| scan_mutation_expr(value, changed)),
        ExprKind::Unary(_, value)
        | ExprKind::Refine { value, .. }
        | ExprKind::Unrefine(value)
        | ExprKind::MakeView { value, .. } => scan_mutation_expr(value, changed),
        ExprKind::Binary(operator, left, right) => {
            if !scan_mutation_expr(left, changed) {
                return false;
            }
            if matches!(operator, BinaryOp::And | BinaryOp::Or) {
                let short_circuit = changed.clone();
                let mut right_changed = short_circuit.clone();
                if scan_mutation_expr(right, &mut right_changed) {
                    changed.extend(right_changed);
                } else {
                    *changed = short_circuit;
                }
                true
            } else {
                scan_mutation_expr(right, changed)
            }
        }
        ExprKind::Block(block) => scan_mutation_block(block, changed),
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            if !scan_mutation_expr(condition, changed) {
                return false;
            }
            let entry = changed.clone();
            let mut then_changed = entry.clone();
            let mut else_changed = entry;
            let then_continues = scan_mutation_block(then_branch, &mut then_changed);
            let else_continues = scan_mutation_block(else_branch, &mut else_changed);
            match (then_continues, else_continues) {
                (true, true) => {
                    then_changed.extend(else_changed);
                    *changed = then_changed;
                    true
                }
                (true, false) => {
                    *changed = then_changed;
                    true
                }
                (false, true) => {
                    *changed = else_changed;
                    true
                }
                (false, false) => false,
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            if !scan_mutation_expr(scrutinee, changed) {
                return false;
            }
            let entry = changed.clone();
            let mut continuing = Vec::new();
            for arm in arms {
                let mut arm_changed = entry.clone();
                if scan_mutation_expr(&arm.value, &mut arm_changed) {
                    continuing.push(arm_changed);
                }
            }
            let Some(mut merged) = continuing.pop() else {
                return false;
            };
            for arm_changed in continuing {
                merged.extend(arm_changed);
            }
            *changed = merged;
            true
        }
        ExprKind::Record { fields, .. } => fields
            .iter()
            .all(|field| scan_mutation_expr(field, changed)),
        ExprKind::Variant { payload, .. } => payload
            .iter()
            .all(|value| scan_mutation_expr(value, changed)),
        ExprKind::Call { arguments, .. } => {
            for argument in arguments {
                match argument {
                    CallArgument::Value(value) => {
                        if !scan_mutation_expr(value, changed) {
                            return false;
                        }
                    }
                    CallArgument::InOut(place) => {
                        changed.insert(place.local);
                    }
                }
            }
            true
        }
        ExprKind::Await { task, .. } => scan_mutation_expr(task, changed),
        ExprKind::Sleep { milliseconds } => scan_mutation_expr(milliseconds, changed),
        ExprKind::TaskJoin { arguments, .. } => arguments
            .iter()
            .all(|argument| scan_mutation_expr(argument, changed)),
    };
    continues && expression.ty != Type::Never
}

fn solve_effects(summaries: Vec<InstanceEffectSummary>) -> Result<EffectPlan, LoweringError> {
    let slot_count = summaries.len();
    let mut indexes = BTreeMap::new();
    for (index, summary) in summaries.iter().enumerate() {
        let identity = summary.key.canonical_identity();
        if indexes.insert(identity, index).is_some() {
            return Err(LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                format!("duplicate effect summary for {}", summary.key),
            ));
        }
    }
    let mut incoming_counts = allocated_slots(slot_count, 0_usize, "reverse-call counts")?;
    let mut effects = allocated_slots(slot_count, Effects::NONE, "effect states")?;
    let mut pending = VecDeque::new();
    pending
        .try_reserve(slot_count)
        .map_err(|error| LoweringError::ResourceLimit {
            code: ResourceLimitCode::ProgramTooLarge,
            message: format!("cannot allocate effect worklist: {error}"),
        })?;

    for (caller, summary) in summaries.iter().enumerate() {
        effects[caller] = summary.local.with_implications();
        if !effects[caller].is_empty() {
            pending.push_back(caller);
        }
    }
    for summary in &summaries {
        for callee in &summary.calls {
            let callee_index = indexes
                .get(&callee.canonical_identity())
                .copied()
                .ok_or_else(|| {
                    LoweringError::defect(
                        LoweringDefectCode::InconsistentPlan,
                        format!("{} calls unplanned {callee}", summary.key),
                    )
                })?;
            incoming_counts[callee_index] = incoming_counts[callee_index]
                .checked_add(1)
                .ok_or_else(|| LoweringError::ResourceLimit {
                    code: ResourceLimitCode::ProgramTooLarge,
                    message: format!("too many calls to {callee}"),
                })?;
        }
    }

    let mut reverse_calls = allocated_slots(slot_count, Vec::new(), "reverse-call graph")?;
    for (index, count) in incoming_counts.into_iter().enumerate() {
        reverse_calls[index]
            .try_reserve_exact(count)
            .map_err(|error| LoweringError::ResourceLimit {
                code: ResourceLimitCode::ProgramTooLarge,
                message: format!(
                    "cannot allocate {count} reverse-call edge(s) for function slot {index}: {error}"
                ),
            })?;
    }
    for (caller, summary) in summaries.iter().enumerate() {
        for callee in &summary.calls {
            let callee_index = indexes
                .get(&callee.canonical_identity())
                .copied()
                .expect("all effect callees were checked before graph allocation");
            reverse_calls[callee_index].push(caller);
        }
    }

    while let Some(callee) = pending.pop_front() {
        for caller in reverse_calls[callee].iter().copied() {
            let propagated = effects[caller].union(effects[callee]).with_implications();
            if propagated != effects[caller] {
                effects[caller] = propagated;
                pending.push_back(caller);
            }
        }
    }

    let entries = summaries
        .into_iter()
        .zip(effects)
        .map(|(summary, effects)| EffectPlanEntry {
            key: summary.key,
            effects,
        })
        .collect();
    Ok(EffectPlan { entries })
}

fn allocated_slots<T: Clone>(
    slot_count: usize,
    value: T,
    purpose: &str,
) -> Result<Vec<T>, LoweringError> {
    let mut slots = Vec::new();
    slots
        .try_reserve_exact(slot_count)
        .map_err(|error| LoweringError::ResourceLimit {
            code: ResourceLimitCode::ProgramTooLarge,
            message: format!("cannot allocate {slot_count} {purpose} slot(s): {error}"),
        })?;
    slots.resize(slot_count, value);
    Ok(slots)
}

fn effect_for(effects: &[Effects], function: InstanceId) -> Result<Effects, LoweringError> {
    effects.get(function.index()).copied().ok_or_else(|| {
        LoweringError::defect(
            LoweringDefectCode::InconsistentPlan,
            format!("LCIR instance {function} has no fixed-point effect"),
        )
    })
}

type EnvironmentRoot = u32;

const EMPTY_ENVIRONMENT: EnvironmentRoot = 0;

#[derive(Clone, Copy)]
enum EnvironmentNode {
    Branch {
        zero: EnvironmentRoot,
        one: EnvironmentRoot,
    },
    Leaf(ValueId),
}

/// Function-local persistent map from MIR locals to their current SSA values.
///
/// A flow carries only a root id. Updating one local copies at most one 32-node
/// radix path, while branch fan-out shares the rest of the map. Comparing two
/// roots skips pointer-identical subtries, so an identity branch does not scan
/// every live local at its join.
struct EnvironmentArena {
    nodes: Vec<EnvironmentNode>,
}

impl EnvironmentArena {
    fn new() -> Self {
        Self { nodes: Vec::new() }
    }

    fn get(&self, mut root: EnvironmentRoot, local: LocalId) -> Option<ValueId> {
        for depth in 0..u32::BITS {
            let EnvironmentNode::Branch { zero, one } = self.node(root)? else {
                return None;
            };
            let bit = u32::BITS - depth - 1;
            root = if local.0 & (1_u32 << bit) == 0 {
                zero
            } else {
                one
            };
        }
        match self.node(root) {
            Some(EnvironmentNode::Leaf(value)) => Some(value),
            Some(EnvironmentNode::Branch { .. }) | None => None,
        }
    }

    fn set(
        &mut self,
        root: EnvironmentRoot,
        local: LocalId,
        value: ValueId,
    ) -> Result<EnvironmentRoot, LoweringError> {
        self.set_at(root, local.0, Some(value), 0)
    }

    fn remove(
        &mut self,
        root: EnvironmentRoot,
        local: LocalId,
    ) -> Result<EnvironmentRoot, LoweringError> {
        self.set_at(root, local.0, None, 0)
    }

    fn set_at(
        &mut self,
        root: EnvironmentRoot,
        key: u32,
        value: Option<ValueId>,
        depth: u32,
    ) -> Result<EnvironmentRoot, LoweringError> {
        if depth == u32::BITS {
            return match value {
                None => Ok(EMPTY_ENVIRONMENT),
                Some(value) if matches!(self.node(root), Some(EnvironmentNode::Leaf(current)) if current == value) => {
                    Ok(root)
                }
                Some(value) => self.push_node(EnvironmentNode::Leaf(value)),
            };
        }

        let (zero, one) = self.children(root);
        let bit = u32::BITS - depth - 1;
        let (next_zero, next_one) = if key & (1_u32 << bit) == 0 {
            (self.set_at(zero, key, value, depth + 1)?, one)
        } else {
            (zero, self.set_at(one, key, value, depth + 1)?)
        };
        if next_zero == EMPTY_ENVIRONMENT && next_one == EMPTY_ENVIRONMENT {
            return Ok(EMPTY_ENVIRONMENT);
        }
        if self.children(root) == (next_zero, next_one) {
            return Ok(root);
        }
        self.push_node(EnvironmentNode::Branch {
            zero: next_zero,
            one: next_one,
        })
    }

    fn changed_locals(
        &self,
        base: EnvironmentRoot,
        alternatives: &[EnvironmentRoot],
    ) -> Vec<LocalId> {
        let mut locals = Vec::new();
        for alternative in alternatives {
            self.collect_differences(base, *alternative, 0, 0, &mut locals);
        }
        locals.sort_unstable_by_key(|local| local.0);
        locals.dedup();
        locals
    }

    fn collect_differences(
        &self,
        left: EnvironmentRoot,
        right: EnvironmentRoot,
        depth: u32,
        prefix: u32,
        locals: &mut Vec<LocalId>,
    ) -> usize {
        if left == right {
            return 0;
        }
        if depth == u32::BITS {
            if self.leaf_value(left) != self.leaf_value(right) {
                locals.push(LocalId(prefix));
            }
            return 1;
        }

        let (left_zero, left_one) = self.children(left);
        let (right_zero, right_one) = self.children(right);
        let zero_visits =
            self.collect_differences(left_zero, right_zero, depth + 1, prefix, locals);
        let bit = u32::BITS - depth - 1;
        let one_visits = self.collect_differences(
            left_one,
            right_one,
            depth + 1,
            prefix | (1_u32 << bit),
            locals,
        );
        1 + zero_visits + one_visits
    }

    fn push_node(&mut self, node: EnvironmentNode) -> Result<EnvironmentRoot, LoweringError> {
        let raw = self
            .nodes
            .len()
            .checked_add(1)
            .ok_or_else(|| LoweringError::ResourceLimit {
                code: ResourceLimitCode::ProgramTooLarge,
                message: "SSA environment exhausted the host address space".into(),
            })?;
        let id = u32::try_from(raw).map_err(|_| LoweringError::ResourceLimit {
            code: ResourceLimitCode::ProgramTooLarge,
            message: "SSA environment exhausted its u32 node-id domain".into(),
        })?;
        self.nodes
            .try_reserve(1)
            .map_err(|error| LoweringError::ResourceLimit {
                code: ResourceLimitCode::ProgramTooLarge,
                message: format!("cannot grow the SSA environment: {error}"),
            })?;
        self.nodes.push(node);
        Ok(id)
    }

    fn node(&self, root: EnvironmentRoot) -> Option<EnvironmentNode> {
        root.checked_sub(1)
            .and_then(|index| usize::try_from(index).ok())
            .and_then(|index| self.nodes.get(index))
            .copied()
    }

    fn children(&self, root: EnvironmentRoot) -> (EnvironmentRoot, EnvironmentRoot) {
        match self.node(root) {
            Some(EnvironmentNode::Branch { zero, one }) => (zero, one),
            Some(EnvironmentNode::Leaf(_)) | None => (EMPTY_ENVIRONMENT, EMPTY_ENVIRONMENT),
        }
    }

    fn leaf_value(&self, root: EnvironmentRoot) -> Option<ValueId> {
        match self.node(root) {
            Some(EnvironmentNode::Leaf(value)) => Some(value),
            Some(EnvironmentNode::Branch { .. }) | None => None,
        }
    }
}

#[derive(Clone, Copy)]
struct Flow {
    block: BlockId,
    env: EnvironmentRoot,
}

enum EvalFlow {
    Continue { flow: Flow, value: ValueId },
    Terminated,
}

enum StatementFlow {
    Continue(Flow),
    Terminated,
}

#[derive(Clone)]
enum CleanupAction {
    Deferred(mir::Block),
    Scoped {
        local: LocalId,
        disposal: mir::ScopedDisposal,
        span: Span,
    },
}

#[derive(Clone, Copy)]
enum ProductConstruction {
    Plain,
    InvariantProven,
}

#[derive(Clone)]
struct LoweredMatchArm {
    source_arm: usize,
    block: BlockId,
    captures: Box<[(LocalId, crate::match_plan::MatchValueId, ValueId)]>,
}

struct InOutArgumentPlan {
    parameter: usize,
    place: PlacePlan,
}

struct FunctionLowerer<'function, 'builder, 'plan> {
    program: &'plan mir::Program,
    source: &'function mir::Function,
    key: &'plan InstanceKey,
    builder: FunctionBuilder<'builder>,
    instances: &'plan InstanceLookup,
    effects: &'plan [Effects],
    match_plans: Option<&'plan BTreeMap<ExprId, MatchPlan>>,
    local_types: BTreeMap<LocalId, Type>,
    inout_locals: Box<[LocalId]>,
    environments: EnvironmentArena,
    fault_block: Option<BlockId>,
    cleanups: Vec<CleanupAction>,
    cleanup_expansions: usize,
}

impl<'function, 'builder, 'plan> FunctionLowerer<'function, 'builder, 'plan> {
    fn new(
        program: &'plan mir::Program,
        source: &'function mir::Function,
        key: &'plan InstanceKey,
        builder: FunctionBuilder<'builder>,
        instances: &'plan InstanceLookup,
        effects: &'plan [Effects],
        match_plans: &'plan BTreeMap<String, BTreeMap<ExprId, MatchPlan>>,
    ) -> Self {
        let local_types = source
            .params
            .iter()
            .chain(&source.locals)
            .map(|local| (local.id, local.ty.clone()))
            .collect();
        let inout_locals: Box<[LocalId]> = if source.receiver == Some(mir::Receiver::Mutable) {
            source
                .params
                .first()
                .map(|receiver| vec![receiver.id].into_boxed_slice())
                .unwrap_or_default()
        } else {
            Box::new([])
        };
        Self {
            program,
            source,
            key,
            builder,
            instances,
            effects,
            match_plans: match_plans.get(&key.canonical_identity()),
            local_types,
            inout_locals,
            environments: EnvironmentArena::new(),
            fault_block: None,
            cleanups: Vec::new(),
            cleanup_expansions: 0,
        }
    }

    fn lower(mut self) -> Result<(), LoweringError> {
        let entry = self.create_block()?;
        self.builder.set_entry(entry).map_err(LoweringError::from)?;
        let mut env = EMPTY_ENVIRONMENT;
        for parameter in &self.source.params {
            let ty = self.type_id(&parameter.ty)?;
            let value = self
                .builder
                .append_block_parameter(entry, ty)
                .map_err(LoweringError::from)?;
            env = self.environments.set(env, parameter.id, value)?;
        }
        let flow = Flow { block: entry, env };
        match self.lower_scoped_block(flow, &self.source.body)? {
            EvalFlow::Continue { flow, value } => self.terminate_exit(
                flow,
                TerminatorKind::Return(value),
                self.block_origin(&self.source.body),
            ),
            EvalFlow::Terminated => Ok(()),
        }
    }

    fn type_id(&self, ty: &Type) -> Result<ValueTypeId, LoweringError> {
        let instantiated = InstanceSubstitution::new(self.program, self.key)
            .instantiate_type(ty)
            .map_err(|error| instantiation_defect(self.source.id, None, error))?;
        self.builder
            .representations()
            .type_id(&instantiated)
            .ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    format!("classified direct type {instantiated:?} has no LCIR type"),
                )
            })
    }

    fn local_type(&self, local: LocalId) -> Result<ValueTypeId, LoweringError> {
        let ty = self.local_types.get(&local).ok_or_else(|| {
            LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                format!(
                    "function #{} references unknown local #{}",
                    self.source.id.0, local.0
                ),
            )
        })?;
        self.type_id(ty)
    }

    fn product_field_type(
        &self,
        aggregate: ValueTypeId,
        field: u32,
    ) -> Result<ValueTypeId, LoweringError> {
        let value_type = self
            .builder
            .representations()
            .value_type(aggregate)
            .ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    format!("missing product value type {aggregate}"),
                )
            })?;
        let crate::Repr::Product(product) = self
            .builder
            .representations()
            .repr(value_type.repr())
            .copied()
            .ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    format!("missing representation for product value type {aggregate}"),
                )
            })?
        else {
            return Err(self.unsupported_reached("projection through a non-product value"));
        };
        self.builder
            .representations()
            .product(product)
            .and_then(|product| {
                usize::try_from(field)
                    .ok()
                    .and_then(|index| product.fields().get(index))
            })
            .copied()
            .ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    format!("product field index {field} is missing from {aggregate}"),
                )
            })
    }

    fn place_plan(&self, place: &mir::Place) -> Result<PlacePlan, LoweringError> {
        PlacePlan::build(
            self.builder.representations(),
            place,
            self.local_type(place.local)?,
        )
        .map_err(|error| {
            LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                format!(
                    "function #{} has an invalid typed place plan for local #{}: {error}",
                    self.source.id.0, place.local.0
                ),
            )
        })
    }

    fn validate_place_plan_identity(&self, plan: &PlacePlan) -> Result<(), LoweringError> {
        let representations = self.builder.representations();
        let matches = |ty, repr| {
            representations
                .value_type(ty)
                .is_some_and(|value_type| value_type.repr() == repr)
        };
        if !matches(plan.root_type(), plan.root_repr())
            || !matches(plan.leaf_type(), plan.leaf_repr())
            || plan.steps().iter().copied().any(|step| {
                !matches(step.parent_type(), step.parent_repr())
                    || !matches(step.field_type(), step.field_repr())
            })
        {
            return Err(LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                format!(
                    "function #{} place plan for local #{} lost exact representation identity",
                    self.source.id.0,
                    plan.local().0
                ),
            ));
        }
        Ok(())
    }

    fn read_place(
        &mut self,
        mut flow: Flow,
        plan: &PlacePlan,
        origin: Origin,
    ) -> Result<EvalFlow, LoweringError> {
        self.validate_place_plan_identity(plan)?;
        let mut value = self
            .environments
            .get(flow.env, plan.local())
            .ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    format!(
                        "function #{} reads unavailable local #{}",
                        self.source.id.0,
                        plan.local().0
                    ),
                )
            })?;
        for step in plan.steps().iter().copied() {
            value = match self.one_instruction(
                flow,
                InstructionKind::ProductExtract {
                    aggregate: value,
                    field: step.field(),
                },
                step.field_type(),
                origin,
            )? {
                EvalFlow::Continue {
                    flow: next_flow,
                    value,
                } => {
                    flow = next_flow;
                    value
                }
                EvalFlow::Terminated => {
                    return Err(LoweringError::defect(
                        LoweringDefectCode::Builder,
                        "typed place extraction unexpectedly terminated",
                    ));
                }
            };
        }
        Ok(EvalFlow::Continue { flow, value })
    }

    fn write_place(
        &mut self,
        mut flow: Flow,
        plan: &PlacePlan,
        value: ValueId,
        origin: Origin,
    ) -> Result<Flow, LoweringError> {
        self.validate_place_plan_identity(plan)?;
        let Some((leaf, prefix)) = plan.steps().split_last() else {
            flow.env = self.environments.set(flow.env, plan.local(), value)?;
            return Ok(flow);
        };
        let root = self
            .environments
            .get(flow.env, plan.local())
            .ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    format!(
                        "function #{} writes unavailable product local #{}",
                        self.source.id.0,
                        plan.local().0
                    ),
                )
            })?;
        let mut aggregate = root;
        let mut parents = Vec::with_capacity(prefix.len());
        for step in prefix.iter().copied() {
            let extracted = match self.one_instruction(
                flow,
                InstructionKind::ProductExtract {
                    aggregate,
                    field: step.field(),
                },
                step.field_type(),
                origin,
            )? {
                EvalFlow::Continue {
                    flow: next_flow,
                    value,
                } => {
                    flow = next_flow;
                    value
                }
                EvalFlow::Terminated => {
                    return Err(LoweringError::defect(
                        LoweringDefectCode::Builder,
                        "typed place reconstruction unexpectedly terminated while extracting",
                    ));
                }
            };
            parents.push((aggregate, step));
            aggregate = extracted;
        }
        let mut rebuilt = match self.one_instruction(
            flow,
            InstructionKind::ProductInsert {
                aggregate,
                field: leaf.field(),
                value,
            },
            leaf.parent_type(),
            origin,
        )? {
            EvalFlow::Continue {
                flow: next_flow,
                value,
            } => {
                flow = next_flow;
                value
            }
            EvalFlow::Terminated => {
                return Err(LoweringError::defect(
                    LoweringDefectCode::Builder,
                    "typed place reconstruction unexpectedly terminated while inserting",
                ));
            }
        };
        for (parent, step) in parents.into_iter().rev() {
            rebuilt = match self.one_instruction(
                flow,
                InstructionKind::ProductInsert {
                    aggregate: parent,
                    field: step.field(),
                    value: rebuilt,
                },
                step.parent_type(),
                origin,
            )? {
                EvalFlow::Continue {
                    flow: next_flow,
                    value,
                } => {
                    flow = next_flow;
                    value
                }
                EvalFlow::Terminated => {
                    return Err(LoweringError::defect(
                        LoweringDefectCode::Builder,
                        "typed place reconstruction unexpectedly terminated while rebuilding",
                    ));
                }
            };
        }
        flow.env = self.environments.set(flow.env, plan.local(), rebuilt)?;
        Ok(flow)
    }

    fn create_block(&mut self) -> Result<BlockId, LoweringError> {
        self.builder.create_block().map_err(LoweringError::from)
    }

    fn terminate(
        &mut self,
        block: BlockId,
        kind: TerminatorKind,
        origin: Origin,
    ) -> Result<(), LoweringError> {
        self.builder
            .terminate(block, Terminator::new(kind, origin))
            .map_err(LoweringError::from)
    }

    fn current_writebacks(
        &self,
        environment: EnvironmentRoot,
    ) -> Result<Vec<ValueId>, LoweringError> {
        self.inout_locals
            .iter()
            .map(|local| {
                self.environments.get(environment, *local).ok_or_else(|| {
                    LoweringError::defect(
                        LoweringDefectCode::InconsistentPlan,
                        format!(
                            "function #{} lost inout local #{} before an exit",
                            self.source.id.0, local.0
                        ),
                    )
                })
            })
            .collect()
    }

    fn terminate_exit(
        &mut self,
        flow: Flow,
        kind: TerminatorKind,
        origin: Origin,
    ) -> Result<(), LoweringError> {
        let writebacks = self.current_writebacks(flow.env)?;
        self.builder
            .terminate(
                flow.block,
                Terminator::with_writebacks(kind, origin, writebacks),
            )
            .map_err(LoweringError::from)
    }

    fn expression_origin(&self, expression: &mir::Expr) -> Origin {
        Origin {
            source_function: self.source.id,
            expression: Some(expression.id),
            span: expression.span,
        }
    }

    fn statement_origin(&self, statement: &mir::Statement) -> Origin {
        Origin {
            source_function: self.source.id,
            expression: None,
            span: statement.span,
        }
    }

    fn block_origin(&self, block: &mir::Block) -> Origin {
        Origin {
            source_function: self.source.id,
            expression: None,
            span: block.span,
        }
    }

    fn fault_block(&mut self) -> Result<BlockId, LoweringError> {
        if let Some(block) = self.fault_block {
            return Ok(block);
        }
        let block = self.create_block()?;
        let mut writebacks = Vec::with_capacity(self.inout_locals.len());
        for local in self.inout_locals.iter().copied() {
            writebacks.push(
                self.builder
                    .append_block_parameter(block, self.local_type(local)?)
                    .map_err(LoweringError::from)?,
            );
        }
        let origin = Origin {
            source_function: self.source.id,
            expression: None,
            span: self.source.span,
        };
        self.builder
            .terminate(
                block,
                Terminator::with_writebacks(TerminatorKind::ResumeFault, origin, writebacks),
            )
            .map_err(LoweringError::from)?;
        self.fault_block = Some(block);
        Ok(block)
    }

    fn fault_target(&mut self, flow: Flow) -> Result<UnwindTarget, LoweringError> {
        if !self.cleanups.is_empty() {
            let block = self.create_block()?;
            let cleanup_flow = self.lower_cleanup_suffix(
                Flow {
                    block,
                    env: flow.env,
                },
                0,
            )?;
            self.terminate_exit(
                cleanup_flow,
                TerminatorKind::ResumeFault,
                Origin {
                    source_function: self.source.id,
                    expression: None,
                    span: self.source.span,
                },
            )?;
            return Ok(UnwindTarget::new(block, []));
        }
        let arguments = self.current_writebacks(flow.env)?;
        Ok(UnwindTarget::new(self.fault_block()?, arguments))
    }

    fn one_instruction(
        &mut self,
        flow: Flow,
        kind: InstructionKind,
        ty: ValueTypeId,
        origin: Origin,
    ) -> Result<EvalFlow, LoweringError> {
        let results = self
            .builder
            .append_instruction(flow.block, kind, &[ty], origin)
            .map_err(LoweringError::from)?;
        let value = results.first().copied().ok_or_else(|| {
            LoweringError::defect(
                LoweringDefectCode::Builder,
                "one-result instruction returned no value",
            )
        })?;
        Ok(EvalFlow::Continue { flow, value })
    }

    fn one_trusted_instruction(
        &mut self,
        flow: Flow,
        kind: InstructionKind,
        ty: ValueTypeId,
        origin: Origin,
    ) -> Result<EvalFlow, LoweringError> {
        let results = self
            .builder
            .append_trusted_instruction(flow.block, kind, &[ty], origin)
            .map_err(LoweringError::from)?;
        let value = results.first().copied().ok_or_else(|| {
            LoweringError::defect(
                LoweringDefectCode::Builder,
                "one-result trusted instruction returned no value",
            )
        })?;
        Ok(EvalFlow::Continue { flow, value })
    }

    fn constant(
        &mut self,
        flow: Flow,
        constant: Constant,
        ty: &Type,
        origin: Origin,
    ) -> Result<EvalFlow, LoweringError> {
        let ty = self.type_id(ty)?;
        self.one_instruction(flow, InstructionKind::Constant(constant), ty, origin)
    }

    fn lower_scoped_block(
        &mut self,
        flow: Flow,
        block: &mir::Block,
    ) -> Result<EvalFlow, LoweringError> {
        let cleanup_base = self.cleanups.len();
        let lowered = self.lower_block(flow, block)?;
        let lowered = match lowered {
            EvalFlow::Continue { flow, value } => EvalFlow::Continue {
                flow: self.lower_cleanup_suffix(flow, cleanup_base)?,
                value,
            },
            EvalFlow::Terminated => EvalFlow::Terminated,
        };
        self.cleanups.truncate(cleanup_base);
        Ok(lowered)
    }

    fn lower_cleanup_suffix(&mut self, mut flow: Flow, base: usize) -> Result<Flow, LoweringError> {
        if base > self.cleanups.len() {
            return Err(LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                "cleanup suffix starts beyond the active lexical cleanup stack",
            ));
        }
        let saved = self.cleanups.clone();
        let lowered = (|| {
            for index in (base..saved.len()).rev() {
                self.consume_cleanup_expansion()?;
                self.cleanups.clear();
                self.cleanups.extend_from_slice(&saved[..index]);
                flow = self.lower_cleanup_action(flow, saved[index].clone())?;
            }
            Ok(flow)
        })();
        self.cleanups = saved;
        lowered
    }

    fn register_cleanup(&mut self, cleanup: CleanupAction) -> Result<(), LoweringError> {
        if self.cleanups.len() >= DIRECT_CLEANUP_MAX_ACTIVE_ACTIONS {
            return Err(LoweringError::ResourceLimit {
                code: ResourceLimitCode::ProgramTooLarge,
                message: format!(
                    "LCIR function #{} exceeds the {DIRECT_CLEANUP_MAX_ACTIVE_ACTIONS}-action direct lexical cleanup depth",
                    self.source.id.0
                ),
            });
        }
        self.cleanups.push(cleanup);
        Ok(())
    }

    fn consume_cleanup_expansion(&mut self) -> Result<(), LoweringError> {
        self.cleanup_expansions =
            self.cleanup_expansions
                .checked_add(1)
                .ok_or_else(|| LoweringError::ResourceLimit {
                    code: ResourceLimitCode::ProgramTooLarge,
                    message: format!(
                        "LCIR function #{} direct lexical cleanup expansion overflowed",
                        self.source.id.0
                    ),
                })?;
        if self.cleanup_expansions > DIRECT_CLEANUP_MAX_EXPANSIONS {
            return Err(LoweringError::ResourceLimit {
                code: ResourceLimitCode::ProgramTooLarge,
                message: format!(
                    "LCIR function #{} exceeds the {DIRECT_CLEANUP_MAX_EXPANSIONS}-action direct lexical cleanup expansion budget",
                    self.source.id.0
                ),
            });
        }
        Ok(())
    }

    fn lower_cleanup_action(
        &mut self,
        flow: Flow,
        cleanup: CleanupAction,
    ) -> Result<Flow, LoweringError> {
        match cleanup {
            CleanupAction::Deferred(block) => match self.lower_scoped_block(flow, &block)? {
                EvalFlow::Continue { flow, .. } => Ok(flow),
                EvalFlow::Terminated => Err(LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    "checked defer cleanup terminated its enclosing function",
                )),
            },
            CleanupAction::Scoped {
                local,
                disposal,
                span,
            } => self.lower_scoped_disposal(flow, local, &disposal, span),
        }
    }

    fn lower_scoped_disposal(
        &mut self,
        flow: Flow,
        local: LocalId,
        disposal: &mir::ScopedDisposal,
        span: Span,
    ) -> Result<Flow, LoweringError> {
        match disposal {
            mir::ScopedDisposal::StaticConcept {
                requirement,
                witness,
                dispatch_type,
            } => self.lower_static_scoped_disposal(
                flow,
                local,
                *requirement,
                witness,
                dispatch_type,
                span,
            ),
            mir::ScopedDisposal::FileClose => {
                self.lower_builtin_scoped_disposal(flow, local, ResourceKind::File, span)
            }
            mir::ScopedDisposal::SocketClose => {
                self.lower_builtin_scoped_disposal(flow, local, ResourceKind::Socket, span)
            }
        }
    }

    fn lower_builtin_scoped_disposal(
        &mut self,
        mut flow: Flow,
        local: LocalId,
        kind: ResourceKind,
        span: Span,
    ) -> Result<Flow, LoweringError> {
        let origin = Origin {
            source_function: self.source.id,
            expression: None,
            span,
        };
        let place = self.place_plan(&mir::Place::local(local))?;
        let EvalFlow::Continue {
            flow: next_flow,
            value: resource,
        } = self.read_place(flow, &place, origin)?
        else {
            return Err(LoweringError::defect(
                LoweringDefectCode::Builder,
                "built-in scoped resource read unexpectedly terminated",
            ));
        };
        flow = next_flow;
        let resource_type = place.leaf_type();
        let unit_type = self.type_id(&Type::Unit)?;

        let normal = self.create_block()?;
        let _unit = self
            .builder
            .append_block_parameter(normal, unit_type)
            .map_err(LoweringError::from)?;
        let normal_resource = self
            .builder
            .append_block_parameter(normal, resource_type)
            .map_err(LoweringError::from)?;
        let normal_flow = self.write_place(
            Flow {
                block: normal,
                env: flow.env,
            },
            &place,
            normal_resource,
            origin,
        )?;

        let fault = self.create_block()?;
        let fault_resource = self
            .builder
            .append_block_parameter(fault, resource_type)
            .map_err(LoweringError::from)?;
        let fault_flow = self.write_place(
            Flow {
                block: fault,
                env: flow.env,
            },
            &place,
            fault_resource,
            origin,
        )?;
        let propagation = self.fault_target(fault_flow)?;
        self.terminate(
            fault_flow.block,
            TerminatorKind::Jump(BlockTarget::new(propagation.block, propagation.arguments)),
            origin,
        )?;
        self.terminate(
            flow.block,
            TerminatorKind::ResourceClose {
                kind,
                resource,
                normal: ResultTarget::new(normal, []),
                fault: UnwindTarget::new(fault, []),
            },
            origin,
        )?;
        Ok(normal_flow)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the statically selected disposal keeps its exact normal and unwind writeback edges together"
    )]
    fn lower_static_scoped_disposal(
        &mut self,
        mut flow: Flow,
        local: LocalId,
        requirement: mir::RequirementId,
        witness: &mir::WitnessRef,
        dispatch_type: &Type,
        span: Span,
    ) -> Result<Flow, LoweringError> {
        let key = InstanceSubstitution::new(self.program, self.key)
            .static_call_key(requirement, witness, dispatch_type, &[], &[])
            .map_err(|error| instantiation_defect(self.source.id, None, error))?;
        let callee_source = self.program.function(key.source()).ok_or_else(|| {
            LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                "statically selected Dispose implementation disappeared",
            )
        })?;
        if callee_source.receiver != Some(mir::Receiver::Mutable)
            || callee_source.params.len() != 1
            || !callee_source.params[0].mutable
            || callee_source.return_ty != Type::Unit
        {
            return Err(LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                "statically selected Dispose implementation lost its canonical mut self signature",
            ));
        }
        let instance = self.instances.get(&key).ok_or_else(|| {
            LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                "statically selected Dispose implementation has no LCIR instance",
            )
        })?;
        let origin = Origin {
            source_function: self.source.id,
            expression: None,
            span,
        };
        let place = self.place_plan(&mir::Place::local(local))?;
        let EvalFlow::Continue {
            flow: next_flow,
            value: receiver,
        } = self.read_place(flow, &place, origin)?
        else {
            return Err(LoweringError::defect(
                LoweringDefectCode::Builder,
                "scoped disposal receiver read unexpectedly terminated",
            ));
        };
        flow = next_flow;
        let unit = self.type_id(&Type::Unit)?;
        let receiver_type = place.leaf_type();
        let effect = effect_for(self.effects, instance)?;
        if !effect.contains(Effects::MAY_FAULT) {
            let results = self
                .builder
                .append_instruction(
                    flow.block,
                    InstructionKind::DirectCall {
                        callee: instance,
                        arguments: Box::new([receiver]),
                    },
                    &[unit, receiver_type],
                    origin,
                )
                .map_err(LoweringError::from)?;
            let writeback = results.get(1).copied().ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::Builder,
                    "infallible scoped disposal produced no receiver writeback",
                )
            })?;
            return self.write_place(flow, &place, writeback, origin);
        }

        let normal = self.create_block()?;
        let _result = self
            .builder
            .append_block_parameter(normal, unit)
            .map_err(LoweringError::from)?;
        let normal_writeback = self
            .builder
            .append_block_parameter(normal, receiver_type)
            .map_err(LoweringError::from)?;
        let normal_flow = self.write_place(
            Flow {
                block: normal,
                env: flow.env,
            },
            &place,
            normal_writeback,
            origin,
        )?;

        let unwind = self.create_block()?;
        let unwind_writeback = self
            .builder
            .append_block_parameter(unwind, receiver_type)
            .map_err(LoweringError::from)?;
        let unwind_flow = self.write_place(
            Flow {
                block: unwind,
                env: flow.env,
            },
            &place,
            unwind_writeback,
            origin,
        )?;
        let propagation = self.fault_target(unwind_flow)?;
        self.terminate(
            unwind_flow.block,
            TerminatorKind::Jump(BlockTarget::new(propagation.block, propagation.arguments)),
            origin,
        )?;
        self.terminate(
            flow.block,
            TerminatorKind::Invoke {
                callee: instance,
                arguments: Box::new([receiver]),
                normal: ResultTarget::new(normal, []),
                unwind: UnwindTarget::new(unwind, []),
            },
            origin,
        )?;
        Ok(normal_flow)
    }

    fn lower_block(
        &mut self,
        mut flow: Flow,
        block: &mir::Block,
    ) -> Result<EvalFlow, LoweringError> {
        for statement in &block.statements {
            flow = match self.lower_statement(flow, statement)? {
                StatementFlow::Continue(flow) => flow,
                StatementFlow::Terminated => return Ok(EvalFlow::Terminated),
            };
        }
        if let Some(tail) = block.tail.as_deref() {
            self.lower_expr(flow, tail)
        } else {
            self.constant(flow, Constant::Unit, &Type::Unit, self.block_origin(block))
        }
    }

    #[allow(clippy::too_many_lines)]
    fn lower_statement(
        &mut self,
        flow: Flow,
        statement: &mir::Statement,
    ) -> Result<StatementFlow, LoweringError> {
        match &statement.kind {
            StatementKind::Let { local, value } => match self.lower_expr(flow, value)? {
                EvalFlow::Continue { mut flow, value } => {
                    flow.env = self.environments.set(flow.env, *local, value)?;
                    Ok(StatementFlow::Continue(flow))
                }
                EvalFlow::Terminated => Ok(StatementFlow::Terminated),
            },
            StatementKind::Scoped {
                local,
                value,
                disposal,
            } => match self.lower_expr(flow, value)? {
                EvalFlow::Continue { mut flow, value } => {
                    flow.env = self.environments.set(flow.env, *local, value)?;
                    self.register_cleanup(CleanupAction::Scoped {
                        local: *local,
                        disposal: disposal.clone(),
                        span: statement.span,
                    })?;
                    Ok(StatementFlow::Continue(flow))
                }
                EvalFlow::Terminated => Ok(StatementFlow::Terminated),
            },
            StatementKind::ForRange {
                local,
                start,
                end,
                body,
            } => self.lower_for_range(flow, *local, start, end, body, statement),
            StatementKind::Assign { place, value } => match self.lower_expr(flow, value)? {
                EvalFlow::Continue { flow, value } => {
                    let plan = self.place_plan(place)?;
                    let flow =
                        self.write_place(flow, &plan, value, self.statement_origin(statement))?;
                    Ok(StatementFlow::Continue(flow))
                }
                EvalFlow::Terminated => Ok(StatementFlow::Terminated),
            },
            StatementKind::LetTuple { locals, value } => match self.lower_expr(flow, value)? {
                EvalFlow::Continue {
                    mut flow,
                    value: aggregate,
                } => {
                    let Type::Tuple(elements) = &value.ty else {
                        return Err(LoweringError::defect(
                            LoweringDefectCode::InconsistentPlan,
                            "checked tuple binding value has a non-tuple type",
                        ));
                    };
                    if elements.len() != locals.len() {
                        return Err(LoweringError::defect(
                            LoweringDefectCode::InconsistentPlan,
                            "checked tuple binding arity does not match its locals",
                        ));
                    }
                    let aggregate_type = self.type_id(&value.ty)?;
                    for (index, (local, element)) in locals.iter().zip(elements).enumerate() {
                        let field = u32::try_from(index).map_err(|_| {
                            LoweringError::defect(
                                LoweringDefectCode::InconsistentPlan,
                                "checked tuple binding has too many elements",
                            )
                        })?;
                        let field_type = self.product_field_type(aggregate_type, field)?;
                        if field_type != self.type_id(element)?
                            || field_type != self.local_type(*local)?
                        {
                            return Err(LoweringError::defect(
                                LoweringDefectCode::InconsistentPlan,
                                "checked tuple binding element type does not match its local",
                            ));
                        }
                        let extracted = match self.one_instruction(
                            flow,
                            InstructionKind::ProductExtract { aggregate, field },
                            field_type,
                            self.statement_origin(statement),
                        )? {
                            EvalFlow::Continue {
                                flow: next_flow,
                                value,
                            } => {
                                flow = next_flow;
                                value
                            }
                            EvalFlow::Terminated => {
                                return Err(LoweringError::defect(
                                    LoweringDefectCode::Builder,
                                    "tuple extraction unexpectedly terminated",
                                ));
                            }
                        };
                        flow.env = self.environments.set(flow.env, *local, extracted)?;
                    }
                    Ok(StatementFlow::Continue(flow))
                }
                EvalFlow::Terminated => Ok(StatementFlow::Terminated),
            },
            StatementKind::Assert { condition } => match self.lower_expr(flow, condition)? {
                EvalFlow::Continue {
                    flow,
                    value: condition,
                } => {
                    let success = self.create_block()?;
                    let fault = self.fault_target(flow)?;
                    self.terminate(
                        flow.block,
                        TerminatorKind::Assert {
                            condition,
                            metadata: ContractFaultMetadata::assertion(statement.span),
                            success: BlockTarget::new(success, []),
                            fault,
                        },
                        self.statement_origin(statement),
                    )?;
                    Ok(StatementFlow::Continue(Flow {
                        block: success,
                        env: flow.env,
                    }))
                }
                EvalFlow::Terminated => Ok(StatementFlow::Terminated),
            },
            StatementKind::Evaluate(expression) => match self.lower_expr(flow, expression)? {
                EvalFlow::Continue { flow, .. } => Ok(StatementFlow::Continue(flow)),
                EvalFlow::Terminated => Ok(StatementFlow::Terminated),
            },
            StatementKind::Return(value) => {
                let origin = self.statement_origin(statement);
                let lowered = if let Some(value) = value {
                    self.lower_expr(flow, value)?
                } else {
                    self.constant(flow, Constant::Unit, &Type::Unit, origin)?
                };
                match lowered {
                    EvalFlow::Continue { flow, value } => {
                        let flow = self.lower_cleanup_suffix(flow, 0)?;
                        self.terminate_exit(flow, TerminatorKind::Return(value), origin)?;
                        Ok(StatementFlow::Terminated)
                    }
                    EvalFlow::Terminated => Ok(StatementFlow::Terminated),
                }
            }
            StatementKind::Defer(cleanup) => {
                self.register_cleanup(CleanupAction::Deferred(cleanup.clone()))?;
                Ok(StatementFlow::Continue(flow))
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn lower_expr(
        &mut self,
        flow: Flow,
        expression: &mir::Expr,
    ) -> Result<EvalFlow, LoweringError> {
        let origin = self.expression_origin(expression);
        match &expression.kind {
            ExprKind::Constant(constant) => {
                let constant = match constant {
                    mir::Constant::Unit => Constant::Unit,
                    mir::Constant::Bool(value) => Constant::Bool(*value),
                    mir::Constant::Int(value) => Constant::Int(*value),
                    mir::Constant::Float(value) => Constant::float(*value),
                    mir::Constant::Text(value) => {
                        return self.one_instruction(
                            flow,
                            InstructionKind::TextLiteral {
                                utf8: value.clone().into_boxed_str(),
                            },
                            self.type_id(&Type::Text)?,
                            origin,
                        );
                    }
                };
                self.constant(flow, constant, &expression.ty, origin)
            }
            ExprKind::Copy(place) | ExprKind::Move(place) => {
                let plan = self.place_plan(place)?;
                if plan.leaf_type() != self.type_id(&expression.ty)? {
                    return Err(LoweringError::defect(
                        LoweringDefectCode::InconsistentPlan,
                        "typed place result does not match its checked MIR expression",
                    ));
                }
                let EvalFlow::Continue { mut flow, value } =
                    self.read_place(flow, &plan, origin)?
                else {
                    return Err(LoweringError::defect(
                        LoweringDefectCode::Builder,
                        "typed place read unexpectedly terminated",
                    ));
                };
                if matches!(expression.kind, ExprKind::Move(_)) {
                    flow.env = self.environments.remove(flow.env, place.local)?;
                }
                Ok(EvalFlow::Continue { flow, value })
            }
            ExprKind::Unary(operator, operand) => {
                let EvalFlow::Continue { flow, value } = self.lower_expr(flow, operand)? else {
                    return Ok(EvalFlow::Terminated);
                };
                let operand_ty = InstanceSubstitution::new(self.program, self.key)
                    .instantiate_type(&operand.ty)
                    .map_err(|error| {
                        instantiation_defect(self.source.id, Some(operand.id), error)
                    })?;
                match (operator, &operand_ty) {
                    (UnaryOp::Not, Type::Bool) => {
                        let ty = self.type_id(&Type::Bool)?;
                        self.one_instruction(flow, InstructionKind::BoolNot { value }, ty, origin)
                    }
                    (UnaryOp::Negate, Type::Float) => {
                        let ty = self.type_id(&Type::Float)?;
                        self.one_instruction(
                            flow,
                            InstructionKind::FloatNegate { value },
                            ty,
                            origin,
                        )
                    }
                    (UnaryOp::Negate, Type::Int) => self.lower_checked_negate(flow, value, origin),
                    _ => Err(self.unsupported_reached("scalar unary operation")),
                }
            }
            ExprKind::Binary(operator @ (BinaryOp::And | BinaryOp::Or), left, right) => {
                self.lower_short_circuit(flow, *operator, left, right, expression)
            }
            ExprKind::Binary(operator, left, right) => {
                let EvalFlow::Continue {
                    flow,
                    value: left_value,
                } = self.lower_expr(flow, left)?
                else {
                    return Ok(EvalFlow::Terminated);
                };
                let EvalFlow::Continue {
                    flow,
                    value: right_value,
                } = self.lower_expr(flow, right)?
                else {
                    return Ok(EvalFlow::Terminated);
                };
                self.lower_binary(
                    flow,
                    *operator,
                    left_value,
                    right_value,
                    &left.ty,
                    expression,
                )
            }
            ExprKind::Block(block) => self.lower_scoped_block(flow, block),
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => self.lower_if(flow, condition, then_branch, else_branch, expression),
            ExprKind::Call {
                target,
                type_arguments,
                arguments,
                witnesses,
            } => self.lower_call(
                flow,
                target,
                type_arguments,
                arguments,
                witnesses,
                expression,
            ),
            ExprKind::Tuple(values) => {
                self.lower_product_values(flow, values, expression, ProductConstruction::Plain)
            }
            ExprKind::List(values) => self.lower_unsupported_values(flow, values, "list value"),
            ExprKind::Match { scrutinee, arms } => {
                self.lower_match(flow, scrutinee, arms, expression)
            }
            ExprKind::Record {
                fields,
                construction,
                ..
            } => {
                let instruction = match construction {
                    mir::ConstructionMode::Plain => ProductConstruction::Plain,
                    mir::ConstructionMode::Proven => ProductConstruction::InvariantProven,
                    mir::ConstructionMode::Runtime => {
                        return Err(self.unsupported_reached("runtime record constraint"));
                    }
                    mir::ConstructionMode::Recheck => {
                        return Err(self.unsupported_reached("serialized record proof recheck"));
                    }
                };
                self.lower_product_values(flow, fields, expression, instruction)
            }
            ExprKind::Variant {
                variant, payload, ..
            } => self.lower_sum_variant(flow, *variant, payload, expression),
            ExprKind::Refine {
                value,
                construction,
                ..
            } => {
                match construction {
                    mir::ConstructionMode::Proven => {}
                    mir::ConstructionMode::Runtime => {
                        return self.lower_unsupported_operand(
                            flow,
                            value,
                            "runtime refinement constraint",
                        );
                    }
                    mir::ConstructionMode::Recheck => {
                        return self.lower_unsupported_operand(
                            flow,
                            value,
                            "serialized refinement proof recheck",
                        );
                    }
                    mir::ConstructionMode::Plain => {
                        return Err(self.unsupported_reached("plain refinement construction"));
                    }
                }
                let EvalFlow::Continue { flow, value } = self.lower_expr(flow, value)? else {
                    return Ok(EvalFlow::Terminated);
                };
                self.one_trusted_instruction(
                    flow,
                    InstructionKind::RefineProven { value },
                    self.type_id(&expression.ty)?,
                    self.expression_origin(expression),
                )
            }
            ExprKind::Unrefine(value) => {
                let EvalFlow::Continue { flow, value } = self.lower_expr(flow, value)? else {
                    return Ok(EvalFlow::Terminated);
                };
                self.one_instruction(
                    flow,
                    InstructionKind::Unrefine { value },
                    self.type_id(&expression.ty)?,
                    self.expression_origin(expression),
                )
            }
            ExprKind::MakeView { value, .. } => {
                self.lower_unsupported_operand(flow, value, "view construction")
            }
            ExprKind::ReborrowView { .. } => Err(self.unsupported_reached("view reborrow")),
            ExprKind::Await { task, .. } => {
                self.lower_unsupported_operand(flow, task, "suspension")
            }
            ExprKind::Sleep { milliseconds } => {
                self.lower_unsupported_operand(flow, milliseconds, "sleep")
            }
            ExprKind::TaskJoin { arguments, .. } => {
                self.lower_unsupported_values(flow, arguments, "task join")
            }
        }
    }

    fn lower_product_values(
        &mut self,
        mut flow: Flow,
        fields: &[mir::Expr],
        expression: &mir::Expr,
        construction: ProductConstruction,
    ) -> Result<EvalFlow, LoweringError> {
        let mut lowered = Vec::with_capacity(fields.len());
        for field in fields {
            let EvalFlow::Continue {
                flow: next_flow,
                value,
            } = self.lower_expr(flow, field)?
            else {
                return Ok(EvalFlow::Terminated);
            };
            flow = next_flow;
            lowered.push(value);
        }
        let instruction = match construction {
            ProductConstruction::Plain => InstructionKind::ProductConstruct {
                fields: lowered.into_boxed_slice(),
            },
            ProductConstruction::InvariantProven => InstructionKind::InvariantRecordProven {
                fields: lowered.into_boxed_slice(),
            },
        };
        match construction {
            ProductConstruction::Plain => self.one_instruction(
                flow,
                instruction,
                self.type_id(&expression.ty)?,
                self.expression_origin(expression),
            ),
            ProductConstruction::InvariantProven => self.one_trusted_instruction(
                flow,
                instruction,
                self.type_id(&expression.ty)?,
                self.expression_origin(expression),
            ),
        }
    }

    fn lower_sum_variant(
        &mut self,
        mut flow: Flow,
        variant: mir::VariantId,
        payload: &[mir::Expr],
        expression: &mir::Expr,
    ) -> Result<EvalFlow, LoweringError> {
        let mut lowered = Vec::with_capacity(payload.len());
        for value in payload {
            let EvalFlow::Continue {
                flow: next_flow,
                value,
            } = self.lower_expr(flow, value)?
            else {
                return Ok(EvalFlow::Terminated);
            };
            flow = next_flow;
            lowered.push(value);
        }
        self.one_instruction(
            flow,
            InstructionKind::SumConstruct {
                variant: variant.0,
                payload: lowered.into_boxed_slice(),
            },
            self.type_id(&expression.ty)?,
            self.expression_origin(expression),
        )
    }

    #[expect(
        clippy::too_many_lines,
        reason = "match setup validates and creates each shared typed arm before one decision walk"
    )]
    fn lower_match(
        &mut self,
        flow: Flow,
        scrutinee: &mir::Expr,
        arms: &[mir::MatchArm],
        expression: &mir::Expr,
    ) -> Result<EvalFlow, LoweringError> {
        let EvalFlow::Continue {
            flow,
            value: scrutinee,
        } = self.lower_expr(flow, scrutinee)?
        else {
            return Ok(EvalFlow::Terminated);
        };
        let plan = self
            .match_plans
            .and_then(|plans| plans.get(&expression.id))
            .cloned()
            .ok_or_else(|| self.unsupported_reached("unplanned pattern match"))?;
        let mut values = vec![None; plan.value_count()];
        // Match-value ids have a separate bounded domain from decision nodes.
        // Grow exactly as payload ids are observed below rather than using
        // source local counts or a universal runtime frame.
        values[0] = Some(scrutinee);
        let mut lowered_arms = BTreeMap::new();
        for (node, decision) in plan.nodes() {
            let MatchNode::Arm { arm, captures } = decision else {
                continue;
            };
            let source_arm = arms.get(*arm).ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    format!("match decision references missing arm {arm}"),
                )
            })?;
            let block = self.create_block()?;
            let mut parameters = Vec::with_capacity(captures.len());
            for (local, capture) in captures.iter().copied() {
                let planned_type = plan.value_type(capture).ok_or_else(|| {
                    LoweringError::defect(
                        LoweringDefectCode::InconsistentPlan,
                        "match capture has no planned type",
                    )
                })?;
                let ty = self.type_id(planned_type)?;
                if self.local_type(local)? != ty {
                    return Err(LoweringError::defect(
                        LoweringDefectCode::InconsistentPlan,
                        format!("match arm {arm} binding type does not match its payload"),
                    ));
                }
                let parameter = self
                    .builder
                    .append_block_parameter(block, ty)
                    .map_err(LoweringError::from)?;
                parameters.push((local, capture, parameter));
            }
            if source_arm.bindings.len() != parameters.len() {
                return Err(LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    format!("match arm {arm} has an inconsistent capture plan"),
                ));
            }
            lowered_arms.insert(
                node,
                LoweredMatchArm {
                    source_arm: *arm,
                    block,
                    captures: parameters.into_boxed_slice(),
                },
            );
        }
        let mut alternatives = Vec::new();
        self.lower_match_node(&plan, plan.root(), flow, &values, expression, &lowered_arms)?;
        for lowered_arm in lowered_arms.values().cloned() {
            let source_arm = arms.get(lowered_arm.source_arm).ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    format!(
                        "match decision references missing arm {}",
                        lowered_arm.source_arm
                    ),
                )
            })?;
            let mut env = flow.env;
            for (local, _, parameter) in lowered_arm.captures.iter().copied() {
                env = self.environments.set(env, local, parameter)?;
            }
            let lowered = self.lower_expr(
                Flow {
                    block: lowered_arm.block,
                    env,
                },
                &source_arm.value,
            )?;
            let lowered = match lowered {
                EvalFlow::Continue { mut flow, value } => {
                    for local in &source_arm.bindings {
                        flow.env = self.environments.remove(flow.env, *local)?;
                    }
                    EvalFlow::Continue { flow, value }
                }
                EvalFlow::Terminated => EvalFlow::Terminated,
            };
            alternatives.push(lowered);
        }
        self.merge_evaluations(
            alternatives,
            flow.env,
            &expression.ty,
            self.expression_origin(expression),
        )
    }

    #[allow(clippy::too_many_arguments)]
    #[expect(
        clippy::too_many_lines,
        reason = "one exhaustive lowering walk keeps decision nodes and their SSA environment changes visibly paired"
    )]
    fn lower_match_node(
        &mut self,
        plan: &MatchPlan,
        node: crate::match_plan::MatchNodeId,
        flow: Flow,
        values: &[Option<ValueId>],
        expression: &mir::Expr,
        lowered_arms: &BTreeMap<crate::match_plan::MatchNodeId, LoweredMatchArm>,
    ) -> Result<(), LoweringError> {
        let decision = plan.node(node).cloned().ok_or_else(|| {
            LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                "match decision references a missing node",
            )
        })?;
        match decision {
            MatchNode::Arm { arm, captures } => {
                let lowered = lowered_arms.get(&node).ok_or_else(|| {
                    LoweringError::defect(
                        LoweringDefectCode::InconsistentPlan,
                        format!("match decision has no shared LCIR block for arm {arm}"),
                    )
                })?;
                if captures.len() != lowered.captures.len()
                    || captures.iter().zip(&lowered.captures).any(
                        |((local, value), (planned_local, planned_value, _))| {
                            local != planned_local || value != planned_value
                        },
                    )
                {
                    return Err(LoweringError::defect(
                        LoweringDefectCode::InconsistentPlan,
                        format!("match arm {arm} changed after its shared block was planned"),
                    ));
                }
                let arguments = captures
                    .iter()
                    .map(|(_, capture)| {
                        values
                            .get(capture.index())
                            .copied()
                            .flatten()
                            .ok_or_else(|| {
                                LoweringError::defect(
                                    LoweringDefectCode::InconsistentPlan,
                                    format!(
                                        "match arm {arm} captures unavailable decision value {}",
                                        capture.index()
                                    ),
                                )
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                self.terminate(
                    flow.block,
                    TerminatorKind::Jump(BlockTarget::new(lowered.block, arguments)),
                    self.expression_origin(expression),
                )
            }
            MatchNode::Constant {
                value,
                constant,
                equal,
                not_equal,
            } => {
                let operand = values
                    .get(value.index())
                    .copied()
                    .flatten()
                    .ok_or_else(|| {
                        LoweringError::defect(
                            LoweringDefectCode::InconsistentPlan,
                            "constant decision operand is unavailable",
                        )
                    })?;
                let ty = plan.value_type(value).ok_or_else(|| {
                    LoweringError::defect(
                        LoweringDefectCode::InconsistentPlan,
                        "constant decision has no planned type",
                    )
                })?;
                let constant = match constant {
                    mir::Constant::Unit => Constant::Unit,
                    mir::Constant::Bool(value) => Constant::Bool(value),
                    mir::Constant::Int(value) => Constant::Int(value),
                    mir::Constant::Float(value) => Constant::float(value),
                    mir::Constant::Text(_) => {
                        return Err(self.unsupported_reached("text pattern"));
                    }
                };
                let EvalFlow::Continue {
                    flow,
                    value: expected,
                } = self.constant(flow, constant, ty, self.expression_origin(expression))?
                else {
                    return Err(LoweringError::defect(
                        LoweringDefectCode::Builder,
                        "match constant unexpectedly terminated",
                    ));
                };
                let instruction = match ty {
                    Type::Bool => InstructionKind::BoolCompare {
                        predicate: BoolPredicate::Equal,
                        left: operand,
                        right: expected,
                    },
                    Type::Int => InstructionKind::IntCompare {
                        predicate: IntPredicate::Equal,
                        left: operand,
                        right: expected,
                    },
                    Type::Float => InstructionKind::FloatCompare {
                        predicate: FloatPredicate::OrderedEqual,
                        left: operand,
                        right: expected,
                    },
                    _ => return Err(self.unsupported_reached("non-scalar constant pattern")),
                };
                let EvalFlow::Continue {
                    flow,
                    value: condition,
                } = self.one_instruction(
                    flow,
                    instruction,
                    self.type_id(&Type::Bool)?,
                    self.expression_origin(expression),
                )?
                else {
                    return Err(LoweringError::defect(
                        LoweringDefectCode::Builder,
                        "match comparison unexpectedly terminated",
                    ));
                };
                let equal_block = self.create_block()?;
                let not_equal_block = self.create_block()?;
                self.terminate(
                    flow.block,
                    TerminatorKind::Branch {
                        condition,
                        then_target: BlockTarget::new(equal_block, []),
                        else_target: BlockTarget::new(not_equal_block, []),
                    },
                    self.expression_origin(expression),
                )?;
                self.lower_match_node(
                    plan,
                    equal,
                    Flow {
                        block: equal_block,
                        env: flow.env,
                    },
                    values,
                    expression,
                    lowered_arms,
                )?;
                self.lower_match_node(
                    plan,
                    not_equal,
                    Flow {
                        block: not_equal_block,
                        env: flow.env,
                    },
                    values,
                    expression,
                    lowered_arms,
                )
            }
            MatchNode::Sum { value, cases } => {
                let scrutinee = values
                    .get(value.index())
                    .copied()
                    .flatten()
                    .ok_or_else(|| {
                        LoweringError::defect(
                            LoweringDefectCode::InconsistentPlan,
                            "sum decision scrutinee is unavailable",
                        )
                    })?;
                let mut lowered_cases = Vec::with_capacity(cases.len());
                let mut case_flows = Vec::with_capacity(cases.len());
                for case in &cases {
                    let block = self.create_block()?;
                    let mut case_values = values.to_vec();
                    for payload in case.payload.iter().copied() {
                        if case_values.len() <= payload.index() {
                            case_values.resize(payload.index().saturating_add(1), None);
                        }
                        let ty = plan.value_type(payload).ok_or_else(|| {
                            LoweringError::defect(
                                LoweringDefectCode::InconsistentPlan,
                                "sum case payload has no planned type",
                            )
                        })?;
                        let parameter = self
                            .builder
                            .append_block_parameter(block, self.type_id(ty)?)
                            .map_err(LoweringError::from)?;
                        case_values[payload.index()] = Some(parameter);
                    }
                    lowered_cases.push(SumCase::new(case.variant, block, []));
                    case_flows.push((case.next, block, case_values));
                }
                self.terminate(
                    flow.block,
                    TerminatorKind::SumSwitch {
                        scrutinee,
                        cases: lowered_cases.into_boxed_slice(),
                    },
                    self.expression_origin(expression),
                )?;
                for (next, block, case_values) in case_flows {
                    self.lower_match_node(
                        plan,
                        next,
                        Flow {
                            block,
                            env: flow.env,
                        },
                        &case_values,
                        expression,
                        lowered_arms,
                    )?;
                }
                Ok(())
            }
        }
    }

    fn lower_unsupported_operand(
        &mut self,
        flow: Flow,
        operand: &mir::Expr,
        operation: &str,
    ) -> Result<EvalFlow, LoweringError> {
        match self.lower_expr(flow, operand)? {
            EvalFlow::Continue { .. } => Err(self.unsupported_reached(operation)),
            EvalFlow::Terminated => Ok(EvalFlow::Terminated),
        }
    }

    fn lower_unsupported_values(
        &mut self,
        mut flow: Flow,
        values: &[mir::Expr],
        operation: &str,
    ) -> Result<EvalFlow, LoweringError> {
        for value in values {
            match self.lower_expr(flow, value)? {
                EvalFlow::Continue {
                    flow: next_flow, ..
                } => flow = next_flow,
                EvalFlow::Terminated => return Ok(EvalFlow::Terminated),
            }
        }
        Err(self.unsupported_reached(operation))
    }

    fn lower_binary(
        &mut self,
        flow: Flow,
        operator: BinaryOp,
        left: ValueId,
        right: ValueId,
        operand_type: &Type,
        expression: &mir::Expr,
    ) -> Result<EvalFlow, LoweringError> {
        let origin = self.expression_origin(expression);
        let operand_type = InstanceSubstitution::new(self.program, self.key)
            .instantiate_type(operand_type)
            .map_err(|error| instantiation_defect(self.source.id, Some(expression.id), error))?;
        if operand_type == Type::Int
            && matches!(
                operator,
                BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide
            )
        {
            let op = match operator {
                BinaryOp::Add => CheckedIntBinaryOp::Add,
                BinaryOp::Subtract => CheckedIntBinaryOp::Subtract,
                BinaryOp::Multiply => CheckedIntBinaryOp::Multiply,
                BinaryOp::Divide => CheckedIntBinaryOp::Divide,
                _ => {
                    return Err(LoweringError::defect(
                        LoweringDefectCode::InconsistentPlan,
                        "non-arithmetic integer operator reached arithmetic lowering",
                    ));
                }
            };
            return self.lower_checked_binary(flow, op, left, right, origin);
        }
        if operand_type == Type::Float
            && matches!(
                operator,
                BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide
            )
        {
            let op = match operator {
                BinaryOp::Add => FloatBinaryOp::Add,
                BinaryOp::Subtract => FloatBinaryOp::Subtract,
                BinaryOp::Multiply => FloatBinaryOp::Multiply,
                BinaryOp::Divide => FloatBinaryOp::Divide,
                _ => {
                    return Err(LoweringError::defect(
                        LoweringDefectCode::InconsistentPlan,
                        "non-arithmetic float operator reached arithmetic lowering",
                    ));
                }
            };
            let ty = self.type_id(&Type::Float)?;
            return self.one_instruction(
                flow,
                InstructionKind::FloatBinary { op, left, right },
                ty,
                origin,
            );
        }

        let ty = self.type_id(&Type::Bool)?;
        let kind = match &operand_type {
            Type::Unit => {
                let value = operator == BinaryOp::Equal;
                return self.constant(flow, Constant::Bool(value), &Type::Bool, origin);
            }
            Type::Bool => InstructionKind::BoolCompare {
                predicate: match operator {
                    BinaryOp::Equal => BoolPredicate::Equal,
                    BinaryOp::NotEqual => BoolPredicate::NotEqual,
                    _ => return Err(self.unsupported_reached("Bool comparison")),
                },
                left,
                right,
            },
            Type::Int => InstructionKind::IntCompare {
                predicate: int_predicate(operator)
                    .ok_or_else(|| self.unsupported_reached("Int comparison"))?,
                left,
                right,
            },
            Type::Float => InstructionKind::FloatCompare {
                predicate: float_predicate(operator)
                    .ok_or_else(|| self.unsupported_reached("Float comparison"))?,
                left,
                right,
            },
            Type::Text => InstructionKind::TextCompare {
                predicate: match operator {
                    BinaryOp::Equal => BoolPredicate::Equal,
                    BinaryOp::NotEqual => BoolPredicate::NotEqual,
                    _ => return Err(self.unsupported_reached("Text comparison")),
                },
                left,
                right,
            },
            _ => return Err(self.unsupported_reached("scalar comparison")),
        };
        self.one_instruction(flow, kind, ty, origin)
    }

    fn lower_checked_negate(
        &mut self,
        flow: Flow,
        value: ValueId,
        origin: Origin,
    ) -> Result<EvalFlow, LoweringError> {
        let normal = self.create_block()?;
        let integer = self.type_id(&Type::Int)?;
        let result = self
            .builder
            .append_block_parameter(normal, integer)
            .map_err(LoweringError::from)?;
        let fault = self.fault_target(flow)?;
        self.terminate(
            flow.block,
            TerminatorKind::CheckedIntNegate {
                value,
                normal: ResultTarget::new(normal, []),
                fault,
            },
            origin,
        )?;
        Ok(EvalFlow::Continue {
            flow: Flow {
                block: normal,
                env: flow.env,
            },
            value: result,
        })
    }

    fn lower_checked_binary(
        &mut self,
        flow: Flow,
        op: CheckedIntBinaryOp,
        left: ValueId,
        right: ValueId,
        origin: Origin,
    ) -> Result<EvalFlow, LoweringError> {
        let normal = self.create_block()?;
        let integer = self.type_id(&Type::Int)?;
        let result = self
            .builder
            .append_block_parameter(normal, integer)
            .map_err(LoweringError::from)?;
        let fault = self.fault_target(flow)?;
        self.terminate(
            flow.block,
            TerminatorKind::CheckedIntBinary {
                op,
                left,
                right,
                normal: ResultTarget::new(normal, []),
                fault,
            },
            origin,
        )?;
        Ok(EvalFlow::Continue {
            flow: Flow {
                block: normal,
                env: flow.env,
            },
            value: result,
        })
    }

    fn merge_environments(
        &mut self,
        base: EnvironmentRoot,
        alternatives: &[EnvironmentRoot],
        join: BlockId,
    ) -> Result<(EnvironmentRoot, Vec<LocalId>), LoweringError> {
        let Some(first_alternative) = alternatives.first().copied() else {
            return Err(LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                "cannot merge an empty set of SSA environments",
            ));
        };
        let locals = self.environments.changed_locals(base, alternatives);
        let mut merged = base;
        let mut varying = Vec::new();
        for local in locals {
            let Some(first) = self.environments.get(first_alternative, local) else {
                merged = self.environments.remove(merged, local)?;
                continue;
            };
            if alternatives
                .iter()
                .any(|environment| self.environments.get(*environment, local).is_none())
            {
                // A move on any incoming path makes the local unavailable
                // after the join. Checked MIR prevents a subsequent read.
                merged = self.environments.remove(merged, local)?;
            } else if alternatives
                .iter()
                .all(|environment| self.environments.get(*environment, local) == Some(first))
            {
                merged = self.environments.set(merged, local, first)?;
            } else {
                let parameter = self
                    .builder
                    .append_block_parameter(join, self.local_type(local)?)
                    .map_err(LoweringError::from)?;
                merged = self.environments.set(merged, local, parameter)?;
                varying.push(local);
            }
        }
        Ok((merged, varying))
    }

    #[allow(clippy::too_many_lines)]
    fn lower_short_circuit(
        &mut self,
        flow: Flow,
        operator: BinaryOp,
        left: &mir::Expr,
        right: &mir::Expr,
        expression: &mir::Expr,
    ) -> Result<EvalFlow, LoweringError> {
        let EvalFlow::Continue {
            flow,
            value: condition,
        } = self.lower_expr(flow, left)?
        else {
            return Ok(EvalFlow::Terminated);
        };
        let evaluate = self.create_block()?;
        let evaluate_flow = self.lower_expr(
            Flow {
                block: evaluate,
                env: flow.env,
            },
            right,
        )?;
        let origin = self.expression_origin(expression);
        let EvalFlow::Continue {
            flow: right_flow,
            value: right_value,
        } = evaluate_flow
        else {
            let continuation = self.create_block()?;
            let skip = BlockTarget::new(continuation, []);
            let evaluate = BlockTarget::new(evaluate, []);
            let (then_target, else_target) = match operator {
                BinaryOp::And => (evaluate, skip),
                BinaryOp::Or => (skip, evaluate),
                _ => return Err(self.unsupported_reached("short-circuit operation")),
            };
            self.terminate(
                flow.block,
                TerminatorKind::Branch {
                    condition,
                    then_target,
                    else_target,
                },
                origin,
            )?;
            return Ok(EvalFlow::Continue {
                flow: Flow {
                    block: continuation,
                    env: flow.env,
                },
                value: condition,
            });
        };

        let join = self.create_block()?;
        let result_varies = condition != right_value;
        let result = if result_varies {
            self.builder
                .append_block_parameter(join, self.type_id(&Type::Bool)?)
                .map_err(LoweringError::from)?
        } else {
            condition
        };
        let incoming_environments = [flow.env, right_flow.env];
        let (env, varying_locals) =
            self.merge_environments(flow.env, &incoming_environments, join)?;

        let mut skip_arguments =
            Vec::with_capacity(usize::from(result_varies) + varying_locals.len());
        let mut right_arguments =
            Vec::with_capacity(usize::from(result_varies) + varying_locals.len());
        if result_varies {
            skip_arguments.push(condition);
            right_arguments.push(right_value);
        }
        for local in &varying_locals {
            skip_arguments.push(self.environments.get(flow.env, *local).ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    format!("short-circuit skip argument lost local #{}", local.0),
                )
            })?);
            right_arguments.push(self.environments.get(right_flow.env, *local).ok_or_else(
                || {
                    LoweringError::defect(
                        LoweringDefectCode::InconsistentPlan,
                        format!("short-circuit RHS argument lost local #{}", local.0),
                    )
                },
            )?);
        }
        let skip = BlockTarget::new(join, skip_arguments);
        let evaluate_target = BlockTarget::new(evaluate, []);
        let (then_target, else_target) = match operator {
            BinaryOp::And => (evaluate_target, skip),
            BinaryOp::Or => (skip, evaluate_target),
            _ => return Err(self.unsupported_reached("short-circuit operation")),
        };
        self.terminate(
            flow.block,
            TerminatorKind::Branch {
                condition,
                then_target,
                else_target,
            },
            origin,
        )?;
        self.terminate(
            right_flow.block,
            TerminatorKind::Jump(BlockTarget::new(join, right_arguments)),
            origin,
        )?;
        Ok(EvalFlow::Continue {
            flow: Flow { block: join, env },
            value: result,
        })
    }

    fn lower_if(
        &mut self,
        flow: Flow,
        condition: &mir::Expr,
        then_branch: &mir::Block,
        else_branch: &mir::Block,
        expression: &mir::Expr,
    ) -> Result<EvalFlow, LoweringError> {
        let EvalFlow::Continue {
            flow,
            value: condition,
        } = self.lower_expr(flow, condition)?
        else {
            return Ok(EvalFlow::Terminated);
        };
        let base_environment = flow.env;
        let then_block = self.create_block()?;
        let else_block = self.create_block()?;
        self.terminate(
            flow.block,
            TerminatorKind::Branch {
                condition,
                then_target: BlockTarget::new(then_block, []),
                else_target: BlockTarget::new(else_block, []),
            },
            self.expression_origin(expression),
        )?;
        let then_flow = self.lower_scoped_block(
            Flow {
                block: then_block,
                env: flow.env,
            },
            then_branch,
        )?;
        let else_flow = self.lower_scoped_block(
            Flow {
                block: else_block,
                env: flow.env,
            },
            else_branch,
        )?;
        self.merge_evaluations(
            [then_flow, else_flow],
            base_environment,
            &expression.ty,
            self.expression_origin(expression),
        )
    }

    fn merge_evaluations(
        &mut self,
        alternatives: impl IntoIterator<Item = EvalFlow>,
        base_environment: EnvironmentRoot,
        result_type: &Type,
        origin: Origin,
    ) -> Result<EvalFlow, LoweringError> {
        let continuing = alternatives
            .into_iter()
            .filter_map(|alternative| match alternative {
                EvalFlow::Continue { flow, value } => Some((flow, value)),
                EvalFlow::Terminated => None,
            })
            .collect::<Vec<_>>();
        let Some((_, first_value)) = continuing.first() else {
            return Ok(EvalFlow::Terminated);
        };
        if continuing.len() == 1 {
            let (flow, value) = continuing.into_iter().next().ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    "single continuing alternative disappeared",
                )
            })?;
            return Ok(EvalFlow::Continue { flow, value });
        }

        let join = self.create_block()?;
        let result_varies = continuing.iter().any(|(_, value)| value != first_value);
        let result = if result_varies {
            self.builder
                .append_block_parameter(join, self.type_id(result_type)?)
                .map_err(LoweringError::from)?
        } else {
            *first_value
        };
        let incoming_environments = continuing
            .iter()
            .map(|(flow, _)| flow.env)
            .collect::<Vec<_>>();
        let (env, varying_locals) =
            self.merge_environments(base_environment, &incoming_environments, join)?;
        for (flow, value) in continuing {
            let mut arguments =
                Vec::with_capacity(usize::from(result_varies) + varying_locals.len());
            if result_varies {
                arguments.push(value);
            }
            for local in &varying_locals {
                arguments.push(self.environments.get(flow.env, *local).ok_or_else(|| {
                    LoweringError::defect(
                        LoweringDefectCode::InconsistentPlan,
                        format!("local #{} disappeared while building a join", local.0),
                    )
                })?);
            }
            self.terminate(
                flow.block,
                TerminatorKind::Jump(BlockTarget::new(join, arguments)),
                origin,
            )?;
        }
        Ok(EvalFlow::Continue {
            flow: Flow { block: join, env },
            value: result,
        })
    }

    #[allow(clippy::too_many_lines)]
    fn lower_for_range(
        &mut self,
        flow: Flow,
        local: LocalId,
        start: &mir::Expr,
        end: &mir::Expr,
        body: &mir::Block,
        statement: &mir::Statement,
    ) -> Result<StatementFlow, LoweringError> {
        let EvalFlow::Continue { flow, value: start } = self.lower_expr(flow, start)? else {
            return Ok(StatementFlow::Terminated);
        };
        let EvalFlow::Continue { flow, value: end } = self.lower_expr(flow, end)? else {
            return Ok(StatementFlow::Terminated);
        };
        let mutations = continuing_mutations(body).unwrap_or_default();
        let carried = mutations
            .iter()
            .copied()
            .filter(|candidate| {
                *candidate != local && self.environments.get(flow.env, *candidate).is_some()
            })
            .collect::<Vec<_>>();
        let header = self.create_block()?;
        let body_block = self.create_block()?;
        let exit = self.create_block()?;
        let integer = self.type_id(&Type::Int)?;
        let current = self
            .builder
            .append_block_parameter(header, integer)
            .map_err(LoweringError::from)?;
        let mut header_env = self.environments.remove(flow.env, local)?;
        let mut preheader_arguments = vec![start];
        for outer_local in &carried {
            let incoming = self
                .environments
                .get(flow.env, *outer_local)
                .ok_or_else(|| {
                    LoweringError::defect(
                        LoweringDefectCode::InconsistentPlan,
                        format!("range lost outer local #{}", outer_local.0),
                    )
                })?;
            let parameter = self
                .builder
                .append_block_parameter(header, self.local_type(*outer_local)?)
                .map_err(LoweringError::from)?;
            header_env = self.environments.set(header_env, *outer_local, parameter)?;
            preheader_arguments.push(incoming);
        }
        let origin = self.statement_origin(statement);
        self.terminate(
            flow.block,
            TerminatorKind::Jump(BlockTarget::new(header, preheader_arguments)),
            origin,
        )?;
        let condition = match self.one_instruction(
            Flow {
                block: header,
                env: header_env,
            },
            InstructionKind::IntCompare {
                predicate: IntPredicate::Less,
                left: current,
                right: end,
            },
            self.type_id(&Type::Bool)?,
            origin,
        )? {
            EvalFlow::Continue { value, .. } => value,
            EvalFlow::Terminated => {
                return Err(LoweringError::defect(
                    LoweringDefectCode::Builder,
                    "range comparison instruction unexpectedly terminated",
                ));
            }
        };
        self.terminate(
            header,
            TerminatorKind::Branch {
                condition,
                then_target: BlockTarget::new(body_block, []),
                else_target: BlockTarget::new(exit, []),
            },
            origin,
        )?;

        let body_env = self.environments.set(header_env, local, current)?;
        let lowered_body = self.lower_scoped_block(
            Flow {
                block: body_block,
                env: body_env,
            },
            body,
        )?;
        if let EvalFlow::Continue {
            flow: body_flow, ..
        } = lowered_body
        {
            let (next_flow, next) = match self.one_instruction(
                body_flow,
                InstructionKind::IntSuccessorBelow {
                    value: current,
                    upper_bound: end,
                    proof: condition,
                },
                integer,
                origin,
            )? {
                EvalFlow::Continue { flow, value } => (flow, value),
                EvalFlow::Terminated => {
                    return Err(LoweringError::defect(
                        LoweringDefectCode::Builder,
                        "range successor instruction unexpectedly terminated",
                    ));
                }
            };
            let mut backedge_arguments = vec![next];
            for outer_local in &carried {
                backedge_arguments.push(
                    self.environments
                        .get(next_flow.env, *outer_local)
                        .ok_or_else(|| {
                            LoweringError::defect(
                                LoweringDefectCode::InconsistentPlan,
                                format!("range body lost outer local #{}", outer_local.0),
                            )
                        })?,
                );
            }
            self.terminate(
                next_flow.block,
                TerminatorKind::Jump(BlockTarget::new(header, backedge_arguments)),
                origin,
            )?;
        }

        Ok(StatementFlow::Continue(Flow {
            block: exit,
            env: header_env,
        }))
    }

    #[allow(clippy::too_many_lines)]
    fn lower_call(
        &mut self,
        mut flow: Flow,
        target: &CallTarget,
        type_arguments: &[Type],
        arguments: &[CallArgument],
        witnesses: &[mir::WitnessRef],
        expression: &mir::Expr,
    ) -> Result<EvalFlow, LoweringError> {
        if let CallTarget::Builtin(builtin) = target {
            return self.lower_text_builtin(flow, *builtin, arguments, expression);
        }
        let mut lowered_arguments = Vec::with_capacity(arguments.len());
        let substitution = InstanceSubstitution::new(self.program, self.key);
        let key = match target {
            CallTarget::Direct(callee) | CallTarget::Inherent(callee) => {
                substitution.call_key(*callee, type_arguments, witnesses)
            }
            CallTarget::StaticConcept {
                requirement,
                witness,
                dispatch_type,
            } => substitution.static_call_key(
                *requirement,
                witness,
                dispatch_type,
                type_arguments,
                witnesses,
            ),
            _ => return Err(self.unsupported_reached("non-direct call")),
        }
        .map_err(|error| instantiation_defect(self.source.id, Some(expression.id), error))?;
        let callee = key.source();
        let callee_source = self.program.function(callee).ok_or_else(|| {
            LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                format!("call target #{} disappeared", callee.0),
            )
        })?;
        let expected_inout = callee_source
            .params
            .iter()
            .enumerate()
            .filter_map(|(index, parameter)| parameter.mutable.then_some(index))
            .collect::<Vec<_>>();
        let mut inout_arguments = Vec::with_capacity(expected_inout.len());
        let origin = self.expression_origin(expression);
        for (index, argument) in arguments.iter().enumerate() {
            match argument {
                CallArgument::Value(argument) => {
                    let EvalFlow::Continue {
                        flow: next_flow,
                        value,
                    } = self.lower_expr(flow, argument)?
                    else {
                        return Ok(EvalFlow::Terminated);
                    };
                    flow = next_flow;
                    lowered_arguments.push(value);
                }
                CallArgument::InOut(place) => {
                    let plan = self.place_plan(place)?;
                    let EvalFlow::Continue {
                        flow: next_flow,
                        value,
                    } = self.read_place(flow, &plan, origin)?
                    else {
                        return Err(LoweringError::defect(
                            LoweringDefectCode::Builder,
                            "typed inout place read unexpectedly terminated",
                        ));
                    };
                    flow = next_flow;
                    lowered_arguments.push(value);
                    inout_arguments.push(InOutArgumentPlan {
                        parameter: index,
                        place: plan,
                    });
                }
            }
        }
        if expected_inout.as_slice()
            != inout_arguments
                .iter()
                .map(|argument| argument.parameter)
                .collect::<Vec<_>>()
                .as_slice()
        {
            return Err(LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                format!(
                    "call to #{} has inout arguments inconsistent with its mutable parameters",
                    callee.0
                ),
            ));
        }
        let instance = self.instances.get(&key).ok_or_else(|| {
            LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                format!("call target #{} has no LCIR instance", callee.0),
            )
        })?;
        let effect = effect_for(self.effects, instance)?;
        let result_type = self.type_id(&expression.ty)?;
        let mut result_types = Vec::with_capacity(1 + inout_arguments.len());
        result_types.push(result_type);
        result_types.extend(
            inout_arguments
                .iter()
                .map(|argument| argument.place.leaf_type()),
        );
        if !effect.contains(Effects::MAY_FAULT) {
            let results = self
                .builder
                .append_instruction(
                    flow.block,
                    InstructionKind::DirectCall {
                        callee: instance,
                        arguments: lowered_arguments.into_boxed_slice(),
                    },
                    &result_types,
                    origin,
                )
                .map_err(LoweringError::from)?;
            let result = results.first().copied().ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::Builder,
                    "direct call produced no source result",
                )
            })?;
            for (argument, writeback) in inout_arguments.iter().zip(results.iter().copied().skip(1))
            {
                flow = self.write_place(flow, &argument.place, writeback, origin)?;
            }
            return Ok(EvalFlow::Continue {
                flow,
                value: result,
            });
        }
        let normal = self.create_block()?;
        let result = self
            .builder
            .append_block_parameter(normal, result_type)
            .map_err(LoweringError::from)?;
        let mut normal_writebacks = Vec::with_capacity(inout_arguments.len());
        for argument in &inout_arguments {
            normal_writebacks.push(
                self.builder
                    .append_block_parameter(normal, argument.place.leaf_type())
                    .map_err(LoweringError::from)?,
            );
        }
        let mut normal_flow = Flow {
            block: normal,
            env: flow.env,
        };
        for (argument, writeback) in inout_arguments.iter().zip(normal_writebacks) {
            normal_flow = self.write_place(normal_flow, &argument.place, writeback, origin)?;
        }
        let unwind = if inout_arguments.is_empty() {
            self.fault_target(flow)?
        } else {
            let bridge = self.create_block()?;
            let mut bridge_writebacks = Vec::with_capacity(inout_arguments.len());
            for argument in &inout_arguments {
                bridge_writebacks.push(
                    self.builder
                        .append_block_parameter(bridge, argument.place.leaf_type())
                        .map_err(LoweringError::from)?,
                );
            }
            let mut bridge_flow = Flow {
                block: bridge,
                env: flow.env,
            };
            for (argument, writeback) in inout_arguments.iter().zip(bridge_writebacks) {
                bridge_flow = self.write_place(bridge_flow, &argument.place, writeback, origin)?;
            }
            let propagation = self.fault_target(bridge_flow)?;
            self.terminate(
                bridge_flow.block,
                TerminatorKind::Jump(BlockTarget::new(propagation.block, propagation.arguments)),
                origin,
            )?;
            UnwindTarget::new(bridge, [])
        };
        self.terminate(
            flow.block,
            TerminatorKind::Invoke {
                callee: instance,
                arguments: lowered_arguments.into_boxed_slice(),
                normal: ResultTarget::new(normal, []),
                unwind,
            },
            origin,
        )?;
        Ok(EvalFlow::Continue {
            flow: normal_flow,
            value: result,
        })
    }

    fn lower_text_builtin(
        &mut self,
        mut flow: Flow,
        builtin: mir::Builtin,
        arguments: &[CallArgument],
        expression: &mir::Expr,
    ) -> Result<EvalFlow, LoweringError> {
        let mut values = Vec::with_capacity(arguments.len());
        for argument in arguments {
            let CallArgument::Value(argument) = argument else {
                return Err(self.unsupported_reached("Text builtin inout argument"));
            };
            let EvalFlow::Continue {
                flow: next_flow,
                value,
            } = self.lower_expr(flow, argument)?
            else {
                return Ok(EvalFlow::Terminated);
            };
            flow = next_flow;
            values.push(value);
        }
        let kind = match (builtin, values.as_slice()) {
            (mir::Builtin::TextLength, [text]) => InstructionKind::TextLength { text: *text },
            (mir::Builtin::TextConcat, [left, right]) => InstructionKind::TextConcat {
                left: *left,
                right: *right,
            },
            (mir::Builtin::TextContains, [text, needle]) => InstructionKind::TextContains {
                text: *text,
                needle: *needle,
            },
            _ => return Err(self.unsupported_reached("unsupported Text builtin")),
        };
        self.one_instruction(
            flow,
            kind,
            self.type_id(&expression.ty)?,
            self.expression_origin(expression),
        )
    }

    fn unsupported_reached(&self, what: &str) -> LoweringError {
        LoweringError::defect(
            LoweringDefectCode::InconsistentPlan,
            format!(
                "support classification admitted {what} in function #{}",
                self.source.id.0
            ),
        )
    }
}

const fn int_predicate(operator: BinaryOp) -> Option<IntPredicate> {
    match operator {
        BinaryOp::Equal => Some(IntPredicate::Equal),
        BinaryOp::NotEqual => Some(IntPredicate::NotEqual),
        BinaryOp::Less => Some(IntPredicate::Less),
        BinaryOp::LessEqual => Some(IntPredicate::LessEqual),
        BinaryOp::Greater => Some(IntPredicate::Greater),
        BinaryOp::GreaterEqual => Some(IntPredicate::GreaterEqual),
        BinaryOp::Add
        | BinaryOp::Subtract
        | BinaryOp::Multiply
        | BinaryOp::Divide
        | BinaryOp::And
        | BinaryOp::Or => None,
    }
}

const fn float_predicate(operator: BinaryOp) -> Option<FloatPredicate> {
    match operator {
        BinaryOp::Equal => Some(FloatPredicate::OrderedEqual),
        BinaryOp::NotEqual => Some(FloatPredicate::UnorderedNotEqual),
        BinaryOp::Less => Some(FloatPredicate::OrderedLess),
        BinaryOp::LessEqual => Some(FloatPredicate::OrderedLessEqual),
        BinaryOp::Greater => Some(FloatPredicate::OrderedGreater),
        BinaryOp::GreaterEqual => Some(FloatPredicate::OrderedGreaterEqual),
        BinaryOp::Add
        | BinaryOp::Subtract
        | BinaryOp::Multiply
        | BinaryOp::Divide
        | BinaryOp::And
        | BinaryOp::Or => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ProgramBrand;

    #[test]
    fn persistent_environment_identity_joins_are_independent_of_live_local_count() {
        const LOCAL_COUNT: u32 = 8_192;
        const IDENTITY_JOIN_COUNT: usize = 8_192;

        let brand = ProgramBrand::fresh();
        let owner = InstanceId::from_index(brand, 0).expect("test instance");
        let value = |raw| ValueId::from_index(owner, raw).expect("test value");
        let mut environments = EnvironmentArena::new();
        let mut root = EMPTY_ENVIRONMENT;
        for raw in 0..LOCAL_COUNT {
            root = environments
                .set(root, LocalId(raw), value(raw as usize))
                .expect("populate persistent environment");
        }

        // One insertion copies no more than its leaf and one branch per key
        // bit. This guards the persistent representation's linear arena bound.
        assert!(environments.nodes.len() <= LOCAL_COUNT as usize * 33);
        let populated_nodes = environments.nodes.len();
        let identity_alternatives = [root, root];
        let mut divergent_nodes_visited = 0;
        for _ in 0..IDENTITY_JOIN_COUNT {
            let mut differences = Vec::new();
            divergent_nodes_visited +=
                environments.collect_differences(root, root, 0, 0, &mut differences);
            assert!(differences.is_empty());
            assert!(
                environments
                    .changed_locals(root, &identity_alternatives)
                    .is_empty()
            );
        }
        assert_eq!(divergent_nodes_visited, 0);
        assert_eq!(environments.nodes.len(), populated_nodes);

        // A real one-local change traverses just its radix path even when the
        // environment contains thousands of unrelated live locals.
        let changed_local = LocalId(LOCAL_COUNT / 2);
        let changed = environments
            .set(root, changed_local, value(LOCAL_COUNT as usize))
            .expect("update one local");
        assert!(environments.nodes.len() <= populated_nodes + 33);
        let mut differences = Vec::new();
        let visits = environments.collect_differences(root, changed, 0, 0, &mut differences);
        assert_eq!(differences, [changed_local]);
        assert!(
            visits <= 33,
            "one changed radix path visited {visits} nodes"
        );
    }

    #[test]
    fn persistent_environment_preserves_high_bit_local_identities() {
        let brand = ProgramBrand::fresh();
        let owner = InstanceId::from_index(brand, 0).expect("test instance");
        let value = |raw| ValueId::from_index(owner, raw).expect("test value");
        let high = LocalId(1_u32 << 31);
        let maximum = LocalId(u32::MAX);
        let mut environments = EnvironmentArena::new();
        let mut base = EMPTY_ENVIRONMENT;
        base = environments
            .set(base, LocalId(0), value(0))
            .expect("set low local");
        base = environments
            .set(base, high, value(1))
            .expect("set high-bit local");
        base = environments
            .set(base, maximum, value(2))
            .expect("set maximum local");

        assert_eq!(environments.get(base, LocalId(0)), Some(value(0)));
        assert_eq!(environments.get(base, high), Some(value(1)));
        assert_eq!(environments.get(base, maximum), Some(value(2)));

        let changed = environments
            .set(base, maximum, value(3))
            .expect("change maximum local");
        assert_eq!(
            environments.changed_locals(base, &[base, changed]),
            [maximum]
        );
        let removed = environments
            .remove(changed, high)
            .expect("remove high-bit local");
        assert_eq!(environments.get(removed, high), None);
        assert_eq!(environments.get(removed, maximum), Some(value(3)));
        assert_eq!(
            environments.changed_locals(base, &[removed]),
            [high, maximum]
        );
    }

    #[test]
    fn direct_never_call_stops_effect_scanning_before_a_dead_callee() {
        let span = Span::default();
        let call = |callee, ty| {
            mir::Expr::new(
                ExprKind::Call {
                    target: CallTarget::Direct(FunctionId(callee)),
                    type_arguments: Vec::new(),
                    arguments: Vec::new(),
                    witnesses: Vec::new(),
                },
                ty,
                span,
            )
        };
        let block = mir::Block {
            statements: vec![
                mir::Statement {
                    kind: StatementKind::Evaluate(call(1, Type::Never)),
                    span,
                },
                mir::Statement {
                    kind: StatementKind::Evaluate(call(2, Type::Unit)),
                    span,
                },
            ],
            tail: None,
            span,
        };
        let mut summary = EffectSummary::default();

        assert!(!scan_effect_block(&block, &mut summary));
        assert_eq!(summary.calls, BTreeSet::from([FunctionId(1)]));
    }

    #[test]
    fn effect_solver_propagates_through_a_long_chain() {
        const FUNCTION_COUNT: u32 = 4_096;
        const LEAF_EFFECTS: Effects = Effects::MAY_FAULT
            .union(Effects::MAY_COLLECT)
            .union(Effects::MAY_SUSPEND)
            .with_implications();
        let mut summaries = Vec::new();
        for raw in 0..FUNCTION_COUNT {
            let mut summary = EffectSummary {
                local: if raw == 0 {
                    LEAF_EFFECTS
                } else {
                    Effects::NONE
                },
                ..EffectSummary::default()
            };
            if raw != 0 {
                summary.calls.insert(FunctionId(raw - 1));
            }
            summaries.push(InstanceEffectSummary::monomorphic(FunctionId(raw), summary));
        }

        let effects = solve_effects(summaries).expect("long-chain effects must solve");
        for (raw, entry) in (0..FUNCTION_COUNT).zip(effects.entries()) {
            assert_eq!(entry.key, InstanceKey::monomorphic(FunctionId(raw)));
            assert_eq!(entry.effects, LEAF_EFFECTS);
        }
    }

    #[test]
    fn effect_solver_propagates_around_a_recursive_scc() {
        let summaries = BTreeMap::from([
            (
                FunctionId(0),
                EffectSummary {
                    local: Effects::MAY_COLLECT.with_implications(),
                    calls: BTreeSet::from([FunctionId(1)]),
                },
            ),
            (
                FunctionId(1),
                EffectSummary {
                    local: Effects::MAY_FAULT,
                    calls: BTreeSet::from([FunctionId(2)]),
                },
            ),
            (
                FunctionId(2),
                EffectSummary {
                    local: Effects::MAY_SUSPEND.with_implications(),
                    calls: BTreeSet::from([FunctionId(0)]),
                },
            ),
            (FunctionId(3), EffectSummary::default()),
        ]);

        let summaries = summaries
            .into_iter()
            .map(|(source, summary)| InstanceEffectSummary::monomorphic(source, summary))
            .collect();
        let effects = solve_effects(summaries).expect("recursive effects must solve");
        assert!(effects.entries()[..3].iter().all(|entry| {
            entry.effects
                == Effects::MAY_FAULT
                    .union(Effects::MAY_COLLECT)
                    .union(Effects::MAY_SUSPEND)
                    .with_implications()
        }));
        assert_eq!(effects.entries()[3].effects, Effects::NONE);
    }

    #[test]
    fn effect_solver_rejects_an_unplanned_callee_as_a_defect() {
        let summaries = vec![InstanceEffectSummary::monomorphic(
            FunctionId(0),
            EffectSummary {
                calls: BTreeSet::from([FunctionId(1)]),
                ..EffectSummary::default()
            },
        )];

        let error = solve_effects(summaries).expect_err("unplanned callee must be rejected");
        assert_eq!(
            error.code(),
            LoweringErrorCode::Defect(LoweringDefectCode::InconsistentPlan)
        );
    }
}
