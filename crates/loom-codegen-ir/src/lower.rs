use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::fmt;

use loom_core::Span;
use loom_mir::{
    self as mir, BinaryOp, CallArgument, CallTarget, Contract, ContractExpr, ContractExprKind,
    ContractValue, ExprId, ExprKind, FunctionId, LocalId, StatementKind, Type, UnaryOp,
    disclosure_type_summary,
};

use crate::aggregate_plan::{
    AggregatePlanner, AggregateRegistrationError, closed_enum_variants, concrete_any_record_fields,
    concrete_record_fields, concrete_refined_base, is_direct_scalar,
};
use crate::dyn_plan::DynConceptPlan;
use crate::instance_closure::{
    InstanceClosureError, InstanceClosureOutcome, InstanceClosureUnsupportedKind,
    InstanceSubstitution, InstantiationError, plan_instance_closure,
};
use crate::match_plan::{MatchNode, MatchPlan, plan_contract_match, plan_match};
use crate::place_plan::{PlaceBudget, PlacePlan, PlaceUse};
use crate::text_plan::TextLiteralBudget;
use crate::{
    ArtifactRootRequest, BlockId, BlockTarget, BoolPredicate, BuildError, BuildErrorCode,
    CheckedArtifact, CheckedIntBinaryOp, Constant, ContractFaultKind, ContractFaultMetadata,
    CoroutinePlan, CoroutineSuspension, Effects, FaultCode, FaultMetadata, FloatBinaryOp,
    FloatPredicate, FunctionBuilder, InstanceId, InstanceKey, InstancePlan, InstanceRole,
    InstructionKind, IntPredicate, Origin, ProgramBuilder, ResourceKind, ResultTarget, Signature,
    SourceRoots, SumCase, TargetLayout, Terminator, TerminatorKind, TestOutcomePlan, UnwindTarget,
    ValueId, ValueTypeId, analyze_source_reachability,
};

const DIRECT_CLEANUP_MAX_ACTIVE_ACTIONS: usize = 1_024;
const DIRECT_CLEANUP_MAX_EXPANSIONS: usize = 65_536;
const DIRECT_EQUALITY_MAX_CFG_NODES: usize = 4_096;

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
    AsyncFunction,
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
    DynamicWitnessSet,
    BuiltinCall,
    GenericInstanceBudget,
    NonRegularGenericRecursion,
    UnresolvedGenericInstantiation,
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
            Self::AsyncFunction => "AsyncFunction",
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
            Self::DynamicWitnessSet => "DynamicWitnessSet",
            Self::BuiltinCall => "BuiltinCall",
            Self::GenericInstanceBudget => "GenericInstanceBudget",
            Self::NonRegularGenericRecursion => "NonRegularGenericRecursion",
            Self::UnresolvedGenericInstantiation => "UnresolvedGenericInstantiation",
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
    let graph = analyze_source_reachability(mir, &selected.source).map_err(|error| {
        LoweringError::defect(
            LoweringDefectCode::SourceGraph,
            format!("checked-MIR reachability failed: {error}"),
        )
    })?;
    let dyn_concepts = DynConceptPlan::from_reachable(mir.as_program(), &graph);
    let closure = match plan_instance_closure(mir.as_program(), &selected.ordered, &dyn_concepts)
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
    let mut classifier = Classifier::new(mir.as_program(), target, &dyn_concepts);
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
        task_handles,
        ..
    } = classifier;
    // Aggregate-contained Text uses the managed-capable pointer provenance mode
    // even when every current value is a compiler literal. This keeps the
    // product representation exact without expanding the separate immortal
    // provenance proof through aggregate construction, projection, and phi
    // flow. A literal pointer remains a valid typed managed-root cell value.
    let managed_text = managed_text || aggregates.uses_text_aggregate_leaf();
    let aggregate_plan = aggregates.finish();
    let root_keys = selected
        .ordered
        .iter()
        .map(|source| {
            let body = InstanceKey::monomorphic(*source);
            if mir
                .function(*source)
                .is_some_and(|function| !function.call_plan.requires.is_empty())
            {
                InstanceKey::checked_root(body)
            } else {
                body
            }
        })
        .collect::<Vec<_>>();
    let mut summaries = closure
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
            Ok(summarize_effects(
                mir.as_program(),
                function,
                key,
                calls,
                &dyn_concepts,
            ))
        })
        .collect::<Result<Vec<_>, LoweringError>>()?;
    for root in &root_keys {
        if root.role() == InstanceRole::CheckedRoot {
            summaries.push(InstanceEffectSummary {
                key: root.clone(),
                local: Effects::MAY_FAULT,
                calls: vec![InstanceKey::monomorphic(root.source())].into_boxed_slice(),
            });
        }
    }
    let effects = solve_effects(summaries)?;
    let unsupported_async = effects.entries().iter().find_map(|entry| {
        let function = mir.function(entry.key.source())?;
        (function.is_async && entry.effects.contains(Effects::MAY_FAULT)).then(|| UnsupportedItem {
            feature: UnsupportedFeature::AsyncFunction,
            function: function.id,
            expression: None,
            span: function.span,
            path: format!("function[{}].fallible_coroutine", function.id.0),
        })
    });
    if let Some(item) = unsupported_async {
        return Ok(LoweringOutcome::Unsupported(SupportReport {
            items: vec![item],
        }));
    }
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
    for task in task_handles {
        builder
            .add_task_handle_type(task)
            .map_err(LoweringError::from)?;
    }
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
                required_type(&builder, &dyn_concepts, &ty)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let result_ty = substitution
            .instantiate_type(&function.return_ty)
            .map_err(|error| instantiation_defect(function.id, None, error))?;
        let result = required_type(&builder, &dyn_concepts, &result_ty)?;
        let inout_params = function
            .params
            .iter()
            .enumerate()
            .filter_map(|(index, parameter)| {
                let instantiated = substitution.instantiate_type(&parameter.ty).ok()?;
                (parameter.mutable || is_mutable_view(&instantiated))
                    .then(|| u32::try_from(index).ok())
                    .flatten()
            })
            .collect::<Vec<_>>();
        let signature = if inout_params.is_empty() {
            Signature::new(params, effect_result(result))
        } else {
            Signature::with_inout_params(params, effect_result(result), inout_params)
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
        let coroutine = if source.is_async {
            let substitution = InstanceSubstitution::new(mir.as_program(), &planned.key);
            let output = substitution
                .instantiate_type(&source.return_ty)
                .map_err(|error| instantiation_defect(source.id, None, error))?;
            let output = required_type(&builder, &dyn_concepts, &output)?;
            let suspensions = source
                .suspension_points
                .iter()
                .map(|point| {
                    let live = point
                        .live_locals
                        .iter()
                        .map(|local| {
                            let ty = source
                                .params
                                .iter()
                                .chain(&source.locals)
                                .find(|candidate| candidate.id == *local)
                                .ok_or_else(|| {
                                    LoweringError::defect(
                                        LoweringDefectCode::InconsistentPlan,
                                        format!(
                                            "async function #{} suspension state {} references missing local #{}",
                                            source.id.0, point.state, local.0
                                        ),
                                    )
                                })?;
                            let ty = substitution.instantiate_type(&ty.ty).map_err(|error| {
                                instantiation_defect(source.id, None, error)
                            })?;
                            required_type(&builder, &dyn_concepts, &ty)
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(CoroutineSuspension::new(point.state, live))
                })
                .collect::<Result<Vec<_>, LoweringError>>()?;
            Some(CoroutinePlan::new(output, suspensions))
        } else {
            None
        };
        let mut function_builder = builder.function(instance).map_err(LoweringError::from)?;
        if let Some(coroutine) = coroutine {
            function_builder
                .set_coroutine_plan(coroutine)
                .map_err(LoweringError::from)?;
        }
        FunctionLowerer::new(
            mir.as_program(),
            source,
            &planned.key,
            function_builder,
            &instances,
            &instance_effects,
            &match_plans,
            &dyn_concepts,
        )
        .lower()?;
    }
    let checked = builder.finish_checked().map_err(|errors| {
        LoweringError::defect(
            LoweringDefectCode::GeneratedProgram,
            format!(
                "compiler-generated LCIR failed validation: {errors}: {:?}",
                errors.as_slice()
            ),
        )
    })?;
    let lowered_roots = root_keys
        .iter()
        .map(|key| {
            instances.get(key).ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    format!("root function #{} has no LCIR instance", key.source().0),
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

fn required_type(
    builder: &ProgramBuilder,
    dyn_concepts: &DynConceptPlan,
    ty: &Type,
) -> Result<ValueTypeId, LoweringError> {
    let physical = dyn_concepts.physical_type(ty).ok_or_else(|| {
        LoweringError::defect(
            LoweringDefectCode::InconsistentPlan,
            format!("classified dynamic type {ty:?} has no unique concrete representation"),
        )
    })?;
    builder.type_id(&physical).ok_or_else(|| {
        LoweringError::defect(
            LoweringDefectCode::InconsistentPlan,
            format!("classified direct type {physical:?} has no LCIR representation"),
        )
    })
}

const fn is_mutable_view(ty: &Type) -> bool {
    matches!(ty, Type::View { mutable: true, .. })
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

fn constant_type(constant: &mir::Constant) -> Type {
    match constant {
        mir::Constant::Unit => Type::Unit,
        mir::Constant::Bool(_) => Type::Bool,
        mir::Constant::Int(_) => Type::Int,
        mir::Constant::Float(_) => Type::Float,
        mir::Constant::Text(_) => Type::Text,
    }
}

fn contract_type_value(value: ContractValue, context: &ContractTypeContext) -> Option<&Type> {
    match value {
        ContractValue::SelfValue => context.receiver.as_ref(),
        ContractValue::Result => context.result.as_ref(),
        ContractValue::Argument(index) => context.arguments.get(index as usize),
        ContractValue::OldSelf => context.old_receiver.as_ref(),
        ContractValue::OldArgument(index) => context
            .old_arguments
            .get(index as usize)
            .and_then(Option::as_ref),
    }
}

fn contract_base_type(program: &mir::Program, ty: &Type) -> Option<Type> {
    let mut current = ty.clone();
    for _ in 0..64 {
        let Type::Nominal(id, _) = &current else {
            return Some(current);
        };
        let definition = program.type_def(*id)?;
        let mir::TypeDefKind::Refined { .. } = &definition.kind else {
            return Some(current);
        };
        current = concrete_refined_base(program, &current)?;
    }
    None
}

fn contract_projected_type(program: &mir::Program, owner: &Type, field: u32) -> Option<Type> {
    let owner = contract_base_type(program, owner)?;
    concrete_any_record_fields(program, &owner)?
        .get(usize::try_from(field).ok()?)
        .cloned()
}

fn is_invariant_record_type(program: &mir::Program, ty: &Type) -> bool {
    let Type::Nominal(id, arguments) = ty else {
        return false;
    };
    program.type_def(*id).is_some_and(|definition| {
        program.prelude.text_map != Some(*id)
            && usize::try_from(definition.type_parameters).ok() == Some(arguments.len())
            && matches!(
                &definition.kind,
                mir::TypeDefKind::Record {
                    invariant: Some(_),
                    ..
                }
            )
    })
}

fn contract_operand(value: ContractValue, context: &ContractContext) -> Option<&ContractOperand> {
    match value {
        ContractValue::SelfValue => context.receiver.as_ref(),
        ContractValue::Result => context.result.as_ref(),
        ContractValue::Argument(index) => context.arguments.get(index as usize),
        ContractValue::OldSelf => context.old_receiver.as_ref(),
        ContractValue::OldArgument(index) => context
            .old_arguments
            .get(index as usize)
            .and_then(Option::as_ref),
    }
}

fn contract_type_context(context: &ContractContext) -> ContractTypeContext {
    ContractTypeContext {
        receiver: context
            .receiver
            .as_ref()
            .map(|value| value.ty.clone())
            .or_else(|| {
                context
                    .record_candidate
                    .as_ref()
                    .map(|candidate| candidate.ty.clone())
            }),
        result: context.result.as_ref().map(|value| value.ty.clone()),
        arguments: context
            .arguments
            .iter()
            .map(|value| value.ty.clone())
            .collect(),
        old_receiver: context.old_receiver.as_ref().map(|value| value.ty.clone()),
        old_arguments: context
            .old_arguments
            .iter()
            .map(|value| value.as_ref().map(|value| value.ty.clone()))
            .collect(),
        bindings: context
            .bindings
            .iter()
            .map(|value| value.ty.clone())
            .collect(),
    }
}

fn contract_expr_type(
    program: &mir::Program,
    expression: &ContractExpr,
    context: &ContractTypeContext,
) -> Option<Type> {
    Some(match &expression.kind {
        ContractExprKind::Constant(constant) => constant_type(constant),
        ContractExprKind::Value(value) => contract_type_value(*value, context)?.clone(),
        ContractExprKind::Binding(index) => context.bindings.get(*index as usize)?.clone(),
        ContractExprKind::Field(owner, field) => contract_projected_type(
            program,
            &contract_expr_type(program, owner, context)?,
            *field,
        )?,
        ContractExprKind::Unary(UnaryOp::Not, _)
        | ContractExprKind::Binary(
            BinaryOp::Equal
            | BinaryOp::NotEqual
            | BinaryOp::Less
            | BinaryOp::LessEqual
            | BinaryOp::Greater
            | BinaryOp::GreaterEqual
            | BinaryOp::And
            | BinaryOp::Or,
            _,
            _,
        )
        | ContractExprKind::IsFinite(_) => Type::Bool,
        ContractExprKind::Unary(UnaryOp::Negate, operand)
        | ContractExprKind::Binary(
            BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide,
            operand,
            _,
        ) => contract_base_type(program, &contract_expr_type(program, operand, context)?)?,
        ContractExprKind::Match { arms, .. } => {
            let arm = arms.first()?;
            let mut nested = context.clone();
            nested.bindings.extend(arm.bindings.iter().cloned());
            contract_expr_type(program, &arm.value, &nested)?
        }
    })
}

fn contract_expr_may_fault(
    program: &mir::Program,
    expression: &ContractExpr,
    context: &ContractTypeContext,
) -> bool {
    match &expression.kind {
        ContractExprKind::Constant(_)
        | ContractExprKind::Value(_)
        | ContractExprKind::Binding(_) => false,
        ContractExprKind::Field(owner, _) | ContractExprKind::IsFinite(owner) => {
            contract_expr_may_fault(program, owner, context)
        }
        ContractExprKind::Unary(operator, operand) => {
            let checked_integer_negate = *operator == UnaryOp::Negate
                && contract_expr_type(program, operand, context)
                    .and_then(|ty| contract_base_type(program, &ty))
                    == Some(Type::Int);
            checked_integer_negate || contract_expr_may_fault(program, operand, context)
        }
        ContractExprKind::Binary(operator, left, right) => {
            let checked_integer_arithmetic = matches!(
                operator,
                BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide
            ) && contract_expr_type(program, left, context)
                .and_then(|ty| contract_base_type(program, &ty))
                == Some(Type::Int);
            checked_integer_arithmetic
                || contract_expr_may_fault(program, left, context)
                || contract_expr_may_fault(program, right, context)
        }
        ContractExprKind::Match { scrutinee, arms } => {
            contract_expr_may_fault(program, scrutinee, context)
                || arms.iter().any(|arm| {
                    let mut nested = context.clone();
                    nested.bindings.extend(arm.bindings.iter().cloned());
                    contract_expr_may_fault(program, &arm.value, &nested)
                })
        }
    }
}

const fn is_scalar_type(ty: &Type) -> bool {
    is_direct_scalar(ty)
}

/// Returns the extra direct types needed to expand equality for one concrete
/// value into a bounded LCIR CFG. Lists use a finite loop and canonical
/// `Option[element]` reads. Re-entering a nominal equality through a List is
/// rejected here because recursively cloning that element CFG would not be a
/// finite lowering plan.
fn direct_structural_equality_dependencies(
    program: &mir::Program,
    ty: &Type,
) -> Option<BTreeSet<Type>> {
    fn visit(
        program: &mir::Program,
        ty: &Type,
        active: &mut BTreeSet<Type>,
        dependencies: &mut BTreeSet<Type>,
        remaining: &mut usize,
    ) -> bool {
        if *remaining == 0 {
            return false;
        }
        *remaining -= 1;
        match ty {
            Type::Unit | Type::Bool | Type::Int | Type::Float | Type::Text => true,
            Type::Tuple(elements) => elements
                .iter()
                .all(|element| visit(program, element, active, dependencies, remaining)),
            Type::List(element) => {
                let Some(option) = program.prelude.option else {
                    return false;
                };
                dependencies.insert(Type::Nominal(option, vec![(**element).clone()]));
                visit(program, element, active, dependencies, remaining)
            }
            Type::Nominal(_, _) => {
                if program
                    .prelude
                    .text_map
                    .is_some_and(|text_map| matches!(ty, Type::Nominal(id, _) if *id == text_map))
                    || !active.insert(ty.clone())
                {
                    return false;
                }
                let result = if let Some(fields) = concrete_any_record_fields(program, ty) {
                    fields
                        .iter()
                        .all(|field| visit(program, field, active, dependencies, remaining))
                } else if let Some(base) = concrete_refined_base(program, ty) {
                    visit(program, &base, active, dependencies, remaining)
                } else if let Some(variants) = closed_enum_variants(program, ty) {
                    let Some(case_cost) = variants.len().checked_mul(variants.len()) else {
                        active.remove(ty);
                        return false;
                    };
                    if *remaining < case_cost {
                        active.remove(ty);
                        return false;
                    }
                    *remaining -= case_cost;
                    variants.iter().all(|variant| {
                        variant
                            .iter()
                            .all(|payload| visit(program, payload, active, dependencies, remaining))
                    })
                } else {
                    false
                };
                active.remove(ty);
                result
            }
            Type::Never
            | Type::Parameter(_)
            | Type::AssociatedProjection { .. }
            | Type::Task(_)
            | Type::TaskOutcome(_)
            | Type::View { .. }
            | Type::Error => false,
        }
    }

    let mut remaining = DIRECT_EQUALITY_MAX_CFG_NODES;
    let mut dependencies = BTreeSet::new();
    visit(
        program,
        ty,
        &mut BTreeSet::new(),
        &mut dependencies,
        &mut remaining,
    )
    .then_some(dependencies)
}

fn runtime_constraint_result_type(program: &mir::Program, success: Type) -> Option<Type> {
    let result = program.prelude.result?;
    let constraint_error = program.prelude.constraint_error?;
    Some(Type::Nominal(
        result,
        vec![success, Type::Nominal(constraint_error, Vec::new())],
    ))
}

fn block_contains_async_cleanup(block: &mir::Block) -> bool {
    block
        .statements
        .iter()
        .any(|statement| match &statement.kind {
            StatementKind::Scoped { .. } | StatementKind::Defer(_) => true,
            StatementKind::ForRange { body, .. } => block_contains_async_cleanup(body),
            StatementKind::Let { value, .. }
            | StatementKind::LetTuple { value, .. }
            | StatementKind::Assign { value, .. }
            | StatementKind::Assert { condition: value }
            | StatementKind::Evaluate(value) => expr_contains_async_cleanup(value),
            StatementKind::Return(value) => value.as_ref().is_some_and(expr_contains_async_cleanup),
        })
        || block
            .tail
            .as_deref()
            .is_some_and(expr_contains_async_cleanup)
}

fn expr_contains_async_cleanup(expression: &mir::Expr) -> bool {
    match &expression.kind {
        ExprKind::Block(block) => block_contains_async_cleanup(block),
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            expr_contains_async_cleanup(condition)
                || block_contains_async_cleanup(then_branch)
                || block_contains_async_cleanup(else_branch)
        }
        ExprKind::Match { scrutinee, arms } => {
            expr_contains_async_cleanup(scrutinee)
                || arms
                    .iter()
                    .any(|arm| expr_contains_async_cleanup(&arm.value))
        }
        ExprKind::Tuple(values)
        | ExprKind::List(values)
        | ExprKind::TaskJoin {
            arguments: values, ..
        }
        | ExprKind::Record { fields: values, .. }
        | ExprKind::Variant {
            payload: values, ..
        } => values.iter().any(expr_contains_async_cleanup),
        ExprKind::Unary(_, value)
        | ExprKind::Refine { value, .. }
        | ExprKind::Unrefine(value)
        | ExprKind::MakeView { value, .. }
        | ExprKind::Await { task: value, .. }
        | ExprKind::Sleep {
            milliseconds: value,
        } => expr_contains_async_cleanup(value),
        ExprKind::Binary(_, left, right) => {
            expr_contains_async_cleanup(left) || expr_contains_async_cleanup(right)
        }
        ExprKind::Call { arguments, .. } => arguments.iter().any(|argument| match argument {
            CallArgument::Value(value) => expr_contains_async_cleanup(value),
            CallArgument::InOut(_) => false,
        }),
        ExprKind::Constant(_)
        | ExprKind::Copy(_)
        | ExprKind::Move(_)
        | ExprKind::ReborrowView { .. } => false,
    }
}

struct Classifier<'program, 'plan> {
    program: &'program mir::Program,
    dyn_concepts: &'plan DynConceptPlan,
    target: TargetLayout,
    items: Vec<UnsupportedItem>,
    aggregates: AggregatePlanner<'program, 'plan>,
    match_plans: BTreeMap<String, BTreeMap<ExprId, MatchPlan>>,
    places: PlaceBudget,
    text_literals: TextLiteralBudget,
    immortal_text: bool,
    managed_text: bool,
    task_handles: BTreeSet<Type>,
}

#[derive(Clone, Copy)]
struct PlaceSite {
    expression: Option<ExprId>,
    span: Span,
}

#[derive(Clone, Debug)]
struct ContractTypeContext {
    receiver: Option<Type>,
    result: Option<Type>,
    arguments: Vec<Type>,
    old_receiver: Option<Type>,
    old_arguments: Vec<Option<Type>>,
    bindings: Vec<Type>,
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

impl<'program, 'plan> Classifier<'program, 'plan> {
    fn new(
        program: &'program mir::Program,
        target: TargetLayout,
        dyn_concepts: &'plan DynConceptPlan,
    ) -> Self {
        Self {
            program,
            dyn_concepts,
            target,
            items: Vec::new(),
            aggregates: AggregatePlanner::new(program, dyn_concepts, target.pointer_bits() == 64),
            match_plans: BTreeMap::new(),
            places: PlaceBudget::default(),
            text_literals: TextLiteralBudget::default(),
            immortal_text: false,
            managed_text: false,
            task_handles: BTreeSet::new(),
        }
    }

    fn supported_value_type(&mut self, ty: &Type) -> bool {
        let Some(ty) = self.dyn_concepts.physical_type(ty) else {
            return false;
        };
        if ty == Type::Text {
            if self.target.pointer_bits() != 64 {
                return false;
            }
            self.immortal_text = true;
            return true;
        }
        if let Type::Task(output) = &ty {
            if self.target.pointer_bits() != 64 || matches!(output.as_ref(), Type::Task(_)) {
                return false;
            }
            if !self.supported_value_type(output) {
                return false;
            }
            self.task_handles.insert(ty.clone());
            return true;
        }
        self.aggregates.supports_value_type(&ty)
    }

    fn supported_coroutine_frame_type(&self, ty: &Type, allow_task_handle: bool) -> bool {
        fn visit(
            program: &mir::Program,
            ty: &Type,
            allow_task_handle: bool,
            active: &mut BTreeSet<Type>,
        ) -> bool {
            match ty {
                Type::Unit | Type::Bool | Type::Int | Type::Float | Type::Text => true,
                Type::Tuple(elements) => elements
                    .iter()
                    .all(|element| visit(program, element, false, active)),
                Type::Task(_) => allow_task_handle,
                Type::Nominal(_, _) => {
                    if !active.insert(ty.clone()) {
                        return false;
                    }
                    let supported = if let Some(fields) = concrete_any_record_fields(program, ty) {
                        fields
                            .iter()
                            .all(|field| visit(program, field, false, active))
                    } else if let Some(base) = concrete_refined_base(program, ty) {
                        visit(program, &base, false, active)
                    } else {
                        false
                    };
                    active.remove(ty);
                    supported
                }
                Type::Never
                | Type::Parameter(_)
                | Type::List(_)
                | Type::AssociatedProjection { .. }
                | Type::TaskOutcome(_)
                | Type::View { .. }
                | Type::Error => false,
            }
        }

        visit(self.program, ty, allow_task_handle, &mut BTreeSet::new())
    }

    fn supported_record_type(&mut self, ty: &Type) -> bool {
        concrete_record_fields(self.program, ty).is_some()
            && self.aggregates.supports_value_type(ty)
    }

    fn supported_expression_type(&mut self, ty: &Type) -> bool {
        matches!(ty, Type::Never) || self.supported_value_type(ty)
    }

    fn supported_equality_type(&mut self, ty: &Type) -> bool {
        let Some(dependencies) = direct_structural_equality_dependencies(self.program, ty) else {
            return false;
        };
        self.supported_value_type(ty)
            && dependencies
                .iter()
                .all(|dependency| self.supported_value_type(dependency))
    }

    fn admit_generated_text_literals(&mut self, literals: &[&str]) -> bool {
        self.text_literals
            .admit_all(literals.iter().map(|literal| literal.len()))
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

    fn call_argument_type(
        &mut self,
        function: &mir::Function,
        key: &InstanceKey,
        argument: &CallArgument,
        expression: &mir::Expr,
        path: &str,
    ) -> Option<Type> {
        let ty = match argument {
            CallArgument::Value(value) => &value.ty,
            CallArgument::InOut(place) if place.projection.is_empty() => {
                Self::local_type(function, place.local)?
            }
            CallArgument::InOut(_) => return None,
        };
        self.instantiated_type(function, key, Some(expression), ty, expression.span, path)
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
        let invariant_receiver = function.receiver == Some(mir::Receiver::Mutable)
            && function
                .params
                .first()
                .is_some_and(|receiver| receiver.id == place.local)
            && is_invariant_record_type(self.program, &ty);
        let invariant_root_access = invariant_receiver
            || (matches!(usage, PlaceUse::Read | PlaceUse::Move)
                && is_invariant_record_type(self.program, &ty));
        for (depth, field) in place.projection.iter().enumerate() {
            let fields = if invariant_root_access && depth == 0 {
                concrete_any_record_fields(self.program, &ty)?
            } else {
                concrete_record_fields(self.program, &ty)?
            };
            let next = usize::try_from(*field)
                .ok()
                .and_then(|index| fields.get(index))
                .cloned()?;
            ty = next;
        }
        self.supported_value_type(&ty).then_some(ty)
    }

    #[allow(clippy::too_many_lines)]
    fn classify_function(&mut self, function: &mir::Function, key: &InstanceKey) {
        let base = format!("function[{}]", function.id.0);
        if !function.is_async && !function.suspension_points.is_empty() {
            self.function_item(UnsupportedFeature::AsyncFunction, function, &base);
        }
        if function.is_async {
            if block_contains_async_cleanup(&function.body) {
                self.function_item(
                    UnsupportedFeature::AsyncFunction,
                    function,
                    &format!("{base}.cleanup"),
                );
            }
            for (index, point) in function.suspension_points.iter().enumerate() {
                if point.state == 0 {
                    self.item(
                        UnsupportedFeature::Suspension,
                        function.id,
                        None,
                        point.span,
                        format!("{base}.suspension_points[{index}].state"),
                    );
                }
                for (live_index, local) in point.live_locals.iter().enumerate() {
                    let supported = Self::local_type(function, *local)
                        .and_then(|ty| {
                            self.instantiated_type(
                                function,
                                key,
                                None,
                                ty,
                                point.span,
                                &format!("{base}.suspension_points[{index}].live[{live_index}]"),
                            )
                        })
                        .is_some_and(|ty| {
                            self.supported_coroutine_frame_type(&ty, true)
                                && self.supported_value_type(&ty)
                        });
                    if !supported {
                        self.item(
                            UnsupportedFeature::Suspension,
                            function.id,
                            None,
                            point.span,
                            format!("{base}.suspension_points[{index}].live[{live_index}]"),
                        );
                    }
                }
            }
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
                .is_some_and(|ty| {
                    self.supported_record_type(&ty)
                        || (is_invariant_record_type(self.program, &ty)
                            && self.aggregates.supports_value_type(&ty))
                });
        if function.receiver == Some(mir::Receiver::Mutable) && !mutable_pod_receiver {
            self.function_item(UnsupportedFeature::MutableReceiver, function, &base);
        }
        for (index, parameter) in function.params.iter().enumerate() {
            let path = format!("{base}.params[{index}]");
            let supported_inout_receiver = index == 0
                && function.receiver == Some(mir::Receiver::Mutable)
                && InstanceSubstitution::new(self.program, key)
                    .instantiate_type(&parameter.ty)
                    .is_ok_and(|ty| {
                        self.supported_record_type(&ty)
                            || (is_invariant_record_type(self.program, &ty)
                                && self.aggregates.supports_value_type(&ty))
                    });
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
                .is_some_and(|ty| {
                    (!function.is_async || self.supported_coroutine_frame_type(&ty, false))
                        && self.supported_value_type(&ty)
                });
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
            .is_some_and(|ty| {
                (!function.is_async || self.supported_coroutine_frame_type(&ty, false))
                    && self.supported_value_type(&ty)
            });
        if !supported_return {
            self.item(
                UnsupportedFeature::SignatureType,
                function.id,
                None,
                function.span,
                return_path,
            );
        }
        self.classify_contracts(function, key, &base);
        // Function-local declarations include values from syntactically dead
        // regions. Reachable expressions below carry their checked types, so
        // classifying uses rather than the whole declaration table keeps DCE
        // exact while still rejecting every executable unsupported value.
        self.visit_block(function, key, &function.body, &format!("{base}.body"));
    }

    fn classify_contracts(&mut self, function: &mir::Function, key: &InstanceKey, base: &str) {
        let substitution = InstanceSubstitution::new(self.program, key);
        let Ok(parameters) = function
            .params
            .iter()
            .map(|parameter| substitution.instantiate_type(&parameter.ty))
            .collect::<Result<Vec<_>, _>>()
        else {
            self.function_item(
                UnsupportedFeature::UnresolvedGenericInstantiation,
                function,
                &format!("{base}.call_plan"),
            );
            return;
        };
        let Ok(result) = substitution.instantiate_type(&function.return_ty) else {
            self.function_item(
                UnsupportedFeature::UnresolvedGenericInstantiation,
                function,
                &format!("{base}.call_plan"),
            );
            return;
        };
        let (receiver, arguments) = if function.receiver.is_some() {
            let Some((receiver, arguments)) = parameters.split_first() else {
                self.function_item(
                    UnsupportedFeature::Contracts,
                    function,
                    &format!("{base}.call_plan.receiver"),
                );
                return;
            };
            (Some(receiver.clone()), arguments.to_vec())
        } else {
            (None, parameters)
        };
        let common = ContractTypeContext {
            receiver: receiver.clone(),
            result: None,
            arguments: arguments.clone(),
            old_receiver: receiver,
            old_arguments: arguments.into_iter().map(Some).collect(),
            bindings: Vec::new(),
        };
        if let Some(contract) = &function.call_plan.receiver_invariant {
            self.classify_contract_expr(
                function,
                key,
                &contract.expression,
                &common,
                &format!("{base}.call_plan.receiver_invariant"),
            );
        }
        for (index, contract) in function.call_plan.requires.iter().enumerate() {
            self.classify_contract_expr(
                function,
                key,
                &contract.expression,
                &common,
                &format!("{base}.call_plan.requires[{index}]"),
            );
        }
        let mut exit = common;
        exit.result = Some(result);
        for (index, contract) in function.call_plan.ensures.iter().enumerate() {
            self.classify_contract_expr(
                function,
                key,
                &contract.expression,
                &exit,
                &format!("{base}.call_plan.ensures[{index}]"),
            );
        }
    }

    fn admit_contract_pattern_text(
        &mut self,
        function: &mir::Function,
        pattern: &mir::Pattern,
        span: Span,
        path: &str,
    ) -> bool {
        let mut pending = vec![pattern];
        while let Some(pattern) = pending.pop() {
            match pattern {
                mir::Pattern::Constant(mir::Constant::Text(value)) => {
                    if self.target.pointer_bits() != 64 || !self.text_literals.admit(value.len()) {
                        self.item(
                            UnsupportedFeature::TextConstant,
                            function.id,
                            None,
                            span,
                            path.to_owned(),
                        );
                        return false;
                    }
                    self.immortal_text = true;
                }
                mir::Pattern::Variant { payload, .. } => pending.extend(payload),
                mir::Pattern::Wildcard | mir::Pattern::Binding | mir::Pattern::Constant(_) => {}
            }
        }
        true
    }

    #[allow(clippy::too_many_lines)]
    fn classify_contract_expr(
        &mut self,
        function: &mir::Function,
        key: &InstanceKey,
        expression: &ContractExpr,
        context: &ContractTypeContext,
        path: &str,
    ) -> Option<Type> {
        let ty = match &expression.kind {
            ContractExprKind::Constant(constant) => {
                if let mir::Constant::Text(value) = constant {
                    if self.target.pointer_bits() != 64 || !self.text_literals.admit(value.len()) {
                        self.item(
                            UnsupportedFeature::TextConstant,
                            function.id,
                            None,
                            expression.span,
                            path.to_owned(),
                        );
                        return None;
                    }
                    self.immortal_text = true;
                }
                constant_type(constant)
            }
            ContractExprKind::Value(value) => contract_type_value(*value, context)?.clone(),
            ContractExprKind::Binding(index) => context.bindings.get(*index as usize)?.clone(),
            ContractExprKind::Field(owner, field) => {
                let owner = self.classify_contract_expr(
                    function,
                    key,
                    owner,
                    context,
                    &format!("{path}.owner"),
                )?;
                contract_projected_type(self.program, &owner, *field)?
            }
            ContractExprKind::Unary(UnaryOp::Not, operand) => {
                self.classify_contract_expr(
                    function,
                    key,
                    operand,
                    context,
                    &format!("{path}.operand"),
                )?;
                Type::Bool
            }
            ContractExprKind::Unary(UnaryOp::Negate, operand) => contract_base_type(
                self.program,
                &self.classify_contract_expr(
                    function,
                    key,
                    operand,
                    context,
                    &format!("{path}.operand"),
                )?,
            )?,
            ContractExprKind::Binary(operator, left, right) => {
                let left = self.classify_contract_expr(
                    function,
                    key,
                    left,
                    context,
                    &format!("{path}.left"),
                )?;
                let right = self.classify_contract_expr(
                    function,
                    key,
                    right,
                    context,
                    &format!("{path}.right"),
                )?;
                let left = contract_base_type(self.program, &left).unwrap_or(left);
                let right = contract_base_type(self.program, &right).unwrap_or(right);
                let supported = left == right
                    && match operator {
                        BinaryOp::Add
                        | BinaryOp::Subtract
                        | BinaryOp::Multiply
                        | BinaryOp::Divide => matches!(left, Type::Int | Type::Float),
                        BinaryOp::And | BinaryOp::Or => left == Type::Bool,
                        BinaryOp::Equal | BinaryOp::NotEqual => self.supported_equality_type(&left),
                        BinaryOp::Less
                        | BinaryOp::LessEqual
                        | BinaryOp::Greater
                        | BinaryOp::GreaterEqual => matches!(left, Type::Int | Type::Float),
                    };
                if !supported {
                    self.item(
                        UnsupportedFeature::Contracts,
                        function.id,
                        None,
                        expression.span,
                        path.to_owned(),
                    );
                    return None;
                }
                if matches!(
                    operator,
                    BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide
                ) {
                    left
                } else {
                    Type::Bool
                }
            }
            ContractExprKind::IsFinite(value) => {
                self.classify_contract_expr(
                    function,
                    key,
                    value,
                    context,
                    &format!("{path}.value"),
                )?;
                Type::Bool
            }
            ContractExprKind::Match { scrutinee, arms } => {
                for (index, arm) in arms.iter().enumerate() {
                    if !self.admit_contract_pattern_text(
                        function,
                        &arm.pattern,
                        expression.span,
                        &format!("{path}.arms[{index}].pattern"),
                    ) {
                        return None;
                    }
                }
                let scrutinee_ty = self.classify_contract_expr(
                    function,
                    key,
                    scrutinee,
                    context,
                    &format!("{path}.scrutinee"),
                )?;
                let planned_scrutinee = contract_base_type(self.program, &scrutinee_ty)
                    .unwrap_or_else(|| scrutinee_ty.clone());
                if plan_contract_match(
                    self.program,
                    &planned_scrutinee,
                    arms,
                    context.bindings.len(),
                )
                .is_none()
                {
                    self.item(
                        UnsupportedFeature::PatternMatch,
                        function.id,
                        None,
                        expression.span,
                        path.to_owned(),
                    );
                    return None;
                }
                let mut result = None;
                for (index, arm) in arms.iter().enumerate() {
                    let mut nested = context.clone();
                    let bindings = arm
                        .bindings
                        .iter()
                        .map(|ty| InstanceSubstitution::new(self.program, key).instantiate_type(ty))
                        .collect::<Result<Vec<_>, _>>()
                        .ok()?;
                    nested.bindings.extend(bindings);
                    let arm_ty = self.classify_contract_expr(
                        function,
                        key,
                        &arm.value,
                        &nested,
                        &format!("{path}.arms[{index}].value"),
                    )?;
                    if result.is_none() {
                        result = Some(arm_ty);
                    }
                }
                result?
            }
        };
        if !self.supported_value_type(&ty) {
            self.item(
                UnsupportedFeature::Contracts,
                function.id,
                None,
                expression.span,
                path.to_owned(),
            );
            return None;
        }
        Some(ty)
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
                if elements.len() > crate::LIST_LITERAL_MAX_ELEMENTS {
                    self.expression_item(UnsupportedFeature::ListValue, function, expression, path);
                }
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
                    let operand_type = self.instantiated_type(
                        function,
                        key,
                        Some(left),
                        &left.ty,
                        left.span,
                        &format!("{path}.left.ty"),
                    );
                    let supported = operand_type.is_some_and(|ty| {
                        if matches!(operator, BinaryOp::Equal | BinaryOp::NotEqual) {
                            self.supported_equality_type(&ty)
                        } else {
                            is_scalar_type(&ty)
                        }
                    });
                    if right_continues && !supported {
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
                if *construction == mir::ConstructionMode::Runtime {
                    let runtime = self
                        .program
                        .type_def(*ty)
                        .and_then(|definition| {
                            (definition.type_parameters == 0).then_some(definition)
                        })
                        .and_then(|definition| match &definition.kind {
                            mir::TypeDefKind::Record {
                                invariant: Some(invariant),
                                ..
                            } => Some((definition.name.clone(), invariant.clone())),
                            _ => None,
                        });
                    let target = Type::Nominal(*ty, Vec::new());
                    let result = runtime_constraint_result_type(self.program, target.clone());
                    let direct_runtime = semantic.as_ref() == Some(&target)
                        && expression_ty.as_ref() == result.as_ref()
                        && self.supported_value_type(&target)
                        && result
                            .as_ref()
                            .is_some_and(|result| self.supported_value_type(result));
                    let contract_supported = runtime.as_ref().is_some_and(|(_, invariant)| {
                        self.classify_contract_expr(
                            function,
                            key,
                            &invariant.expression,
                            &ContractTypeContext {
                                receiver: Some(target.clone()),
                                result: None,
                                arguments: Vec::new(),
                                old_receiver: None,
                                old_arguments: Vec::new(),
                                bindings: Vec::new(),
                            },
                            &format!("{path}.runtime_invariant"),
                        ) == Some(Type::Bool)
                    });
                    if !direct_runtime || !contract_supported {
                        self.expression_item(
                            UnsupportedFeature::NominalValue,
                            function,
                            expression,
                            path,
                        );
                    } else if let Some((name, invariant)) = runtime.as_ref() {
                        let summary = disclosure_type_summary(self.program, &target);
                        if !self.admit_generated_text_literals(&[
                            name,
                            "InvariantViolation",
                            &invariant.code,
                            &summary,
                        ]) {
                            self.expression_item(
                                UnsupportedFeature::TextConstant,
                                function,
                                expression,
                                path,
                            );
                        }
                    }
                    return expression.ty != Type::Never;
                }
                if *construction == mir::ConstructionMode::Recheck {
                    let invariant = self
                        .program
                        .type_def(*ty)
                        .and_then(|definition| {
                            (definition.type_parameters == 0).then_some(definition)
                        })
                        .and_then(|definition| match &definition.kind {
                            mir::TypeDefKind::Record {
                                invariant: Some(invariant),
                                ..
                            } => Some(invariant.clone()),
                            _ => None,
                        });
                    let direct_recheck = semantic.as_ref() == Some(&Type::Nominal(*ty, Vec::new()))
                        && expression_ty.as_ref() == Some(&Type::Nominal(*ty, Vec::new()))
                        && expression_ty
                            .as_ref()
                            .is_some_and(|ty| self.supported_value_type(ty));
                    let contract_supported = invariant.is_some_and(|invariant| {
                        self.classify_contract_expr(
                            function,
                            key,
                            &invariant.expression,
                            &ContractTypeContext {
                                receiver: Some(Type::Nominal(*ty, Vec::new())),
                                result: None,
                                arguments: Vec::new(),
                                old_receiver: None,
                                old_arguments: Vec::new(),
                                bindings: Vec::new(),
                            },
                            &format!("{path}.recheck_invariant"),
                        ) == Some(Type::Bool)
                    });
                    if !direct_recheck || !contract_supported {
                        self.expression_item(
                            UnsupportedFeature::SerializedProofRecheck,
                            function,
                            expression,
                            path,
                        );
                    }
                    return expression.ty != Type::Never;
                }
                let direct_product = expression_ty == semantic
                    && expression_ty
                        .as_ref()
                        .is_some_and(|ty| self.supported_value_type(ty))
                    && self.program.type_def(*ty).is_some_and(|definition| {
                        semantic.as_ref().is_some_and(|semantic| {
                            concrete_any_record_fields(self.program, semantic).is_some()
                        }) && matches!(
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
                if *construction == mir::ConstructionMode::Runtime {
                    let runtime = self
                        .program
                        .type_def(*ty)
                        .and_then(|definition| {
                            (definition.type_parameters == 0).then_some(definition)
                        })
                        .and_then(|definition| match &definition.kind {
                            mir::TypeDefKind::Refined { base, predicate } => {
                                Some((definition.name.clone(), base.clone(), predicate.clone()))
                            }
                            _ => None,
                        });
                    let target = Type::Nominal(*ty, Vec::new());
                    let result = runtime_constraint_result_type(self.program, target.clone());
                    let direct_runtime = expression_ty.as_ref() == result.as_ref()
                        && value_ty.as_ref() == runtime.as_ref().map(|(_, base, _)| base)
                        && self.supported_value_type(&target)
                        && result
                            .as_ref()
                            .is_some_and(|result| self.supported_value_type(result));
                    let contract_supported =
                        runtime.as_ref().is_some_and(|(_, base, predicate)| {
                            self.classify_contract_expr(
                                function,
                                key,
                                &predicate.expression,
                                &ContractTypeContext {
                                    receiver: Some(base.clone()),
                                    result: None,
                                    arguments: Vec::new(),
                                    old_receiver: None,
                                    old_arguments: Vec::new(),
                                    bindings: Vec::new(),
                                },
                                &format!("{path}.runtime_predicate"),
                            ) == Some(Type::Bool)
                        });
                    if !direct_runtime || !contract_supported {
                        self.expression_item(
                            UnsupportedFeature::RefinedValue,
                            function,
                            expression,
                            path,
                        );
                    } else if let Some((name, base, predicate)) = runtime.as_ref() {
                        let summary = disclosure_type_summary(self.program, base);
                        if !self.admit_generated_text_literals(&[
                            name,
                            "ConstraintViolation",
                            &predicate.code,
                            &summary,
                        ]) {
                            self.expression_item(
                                UnsupportedFeature::TextConstant,
                                function,
                                expression,
                                path,
                            );
                        }
                    }
                    return expression.ty != Type::Never;
                }
                if *construction == mir::ConstructionMode::Recheck {
                    let refined = self
                        .program
                        .type_def(*ty)
                        .and_then(|definition| {
                            (definition.type_parameters == 0).then_some(definition)
                        })
                        .and_then(|definition| match &definition.kind {
                            mir::TypeDefKind::Refined { base, predicate } => {
                                Some((base.clone(), predicate.clone()))
                            }
                            _ => None,
                        });
                    let direct_recheck = expression_ty.as_ref()
                        == Some(&Type::Nominal(*ty, Vec::new()))
                        && expression_ty
                            .as_ref()
                            .is_some_and(|ty| self.supported_value_type(ty));
                    let contract_supported = refined.is_some_and(|(base, predicate)| {
                        value_ty.as_ref() == Some(&base)
                            && self.classify_contract_expr(
                                function,
                                key,
                                &predicate.expression,
                                &ContractTypeContext {
                                    receiver: Some(base),
                                    result: None,
                                    arguments: Vec::new(),
                                    old_receiver: None,
                                    old_arguments: Vec::new(),
                                    bindings: Vec::new(),
                                },
                                &format!("{path}.recheck_predicate"),
                            ) == Some(Type::Bool)
                    });
                    if !direct_recheck || !contract_supported {
                        self.expression_item(
                            UnsupportedFeature::SerializedProofRecheck,
                            function,
                            expression,
                            path,
                        );
                    }
                    return expression.ty != Type::Never;
                }
                let proven = *construction == mir::ConstructionMode::Proven
                    && matches!(expression_ty.as_ref(), Some(Type::Nominal(id, _)) if id == ty)
                    && expression_ty
                        .as_ref()
                        .is_some_and(|ty| self.supported_value_type(ty))
                    && expression_ty.as_ref().is_some_and(|refined| {
                        concrete_refined_base(self.program, refined).as_ref() == value_ty.as_ref()
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
                let supported = value_ty.as_ref().is_some_and(|refined| {
                    concrete_refined_base(self.program, refined).as_ref() == expression_ty.as_ref()
                }) && value_ty
                    .as_ref()
                    .is_some_and(|ty| self.supported_value_type(ty))
                    && expression_ty
                        .as_ref()
                        .is_some_and(|ty| self.supported_value_type(ty));
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
                    CallTarget::Dynamic { requirement } => arguments
                        .first()
                        .and_then(|argument| {
                            self.call_argument_type(
                                function,
                                key,
                                argument,
                                expression,
                                &format!("{path}.arguments[0].ty"),
                            )
                        })
                        .and_then(|receiver| {
                            self.dyn_concepts.choice(&receiver).and_then(|choice| {
                                InstanceSubstitution::new(self.program, key)
                                    .static_call_key(
                                        *requirement,
                                        &mir::WitnessRef::Concrete(choice.witness()),
                                        choice.concrete(),
                                        &[],
                                        &[],
                                    )
                                    .ok()
                            })
                        }),
                    CallTarget::Builtin(_) => None,
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
                if callee_key.as_ref().is_some_and(|callee_key| {
                    self.program
                        .function(callee_key.source())
                        .is_some_and(|callee| {
                            callee.is_async
                                && (!function.is_async
                                    || callee.params.iter().any(|parameter| parameter.mutable)
                                    || !callee.call_plan.requires.is_empty())
                        })
                }) {
                    self.expression_item(
                        UnsupportedFeature::AsyncFunction,
                        function,
                        expression,
                        &format!("{path}.target"),
                    );
                }
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
                            let allowed = if matches!(
                                target,
                                CallTarget::Builtin(mir::Builtin::ListAdd)
                            ) {
                                index == 0
                                    && place_type.as_ref().is_some_and(|ty| {
                                        matches!(ty, Type::List(_)) && self.supported_value_type(ty)
                                    })
                            } else if matches!(target, CallTarget::Dynamic { requirement }
                                    if self.program.requirement(*requirement).is_some_and(|requirement| requirement.receiver == Some(mir::Receiver::Mutable)))
                            {
                                index == 0
                                    && place_type.as_ref().is_some_and(|ty| {
                                        if self.dyn_concepts.finite(ty).is_some() {
                                            self.supported_value_type(ty)
                                        } else {
                                            let physical = self
                                                .dyn_concepts
                                                .physical_type(ty)
                                                .unwrap_or_else(|| ty.clone());
                                            mutable_receiver.as_ref() == Some(&physical)
                                                && (self.supported_record_type(&physical)
                                                    || (is_invariant_record_type(
                                                        self.program,
                                                        &physical,
                                                    ) && self
                                                        .aggregates
                                                        .supports_value_type(&physical)))
                                        }
                                    })
                            } else {
                                let physical_place = place_type
                                    .as_ref()
                                    .and_then(|ty| self.dyn_concepts.physical_type(ty));
                                index == 0
                                    && mutable_receiver.as_ref() == physical_place.as_ref()
                                    && place_type.as_ref().is_some_and(|ty| {
                                        let physical = self
                                            .dyn_concepts
                                            .physical_type(ty)
                                            .unwrap_or_else(|| ty.clone());
                                        self.supported_record_type(&physical)
                                            || (is_invariant_record_type(self.program, &physical)
                                                && self.aggregates.supports_value_type(&physical))
                                    })
                            };
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
                    CallTarget::Dynamic { .. }
                        if callee_key.is_some()
                            || arguments.first().is_some_and(|argument| {
                                self.call_argument_type(
                                    function,
                                    key,
                                    argument,
                                    expression,
                                    &format!("{path}.arguments[0].ty"),
                                )
                                .is_some_and(|receiver| {
                                    self.dyn_concepts.finite(&receiver).is_some()
                                })
                            }) =>
                    {
                        None
                    }
                    CallTarget::Dynamic { .. } => Some(UnsupportedFeature::DynamicWitnessSet),
                    CallTarget::Builtin(
                        mir::Builtin::TextLength
                        | mir::Builtin::TextContains
                        | mir::Builtin::TextGet
                        | mir::Builtin::TextConcat
                        | mir::Builtin::ListAdd
                        | mir::Builtin::ListLength
                        | mir::Builtin::ListGet
                        | mir::Builtin::TextMapNew
                        | mir::Builtin::TextMapInsert
                        | mir::Builtin::TextMapLength
                        | mir::Builtin::TextMapGet
                        | mir::Builtin::IsFinite
                        | mir::Builtin::ParseInt
                        | mir::Builtin::ParseFloat
                        | mir::Builtin::FormatFloat
                        | mir::Builtin::DurationMilliseconds
                        | mir::Builtin::DurationAsMilliseconds,
                    ) => {
                        if matches!(
                            target,
                            CallTarget::Builtin(
                                mir::Builtin::TextConcat
                                    | mir::Builtin::TextGet
                                    | mir::Builtin::FormatFloat
                                    | mir::Builtin::TextMapNew
                                    | mir::Builtin::TextMapInsert
                                    | mir::Builtin::TextMapLength
                                    | mir::Builtin::TextMapGet
                            )
                        ) {
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
            ExprKind::MakeView {
                value,
                writeback,
                witness,
                mutable,
                ..
            } => {
                if !self.visit_expr(function, key, value, &format!("{path}.value")) {
                    return false;
                }
                let view = self.instantiated_type(
                    function,
                    key,
                    Some(expression),
                    &expression.ty,
                    expression.span,
                    &format!("{path}.ty"),
                );
                let concrete = self.instantiated_type(
                    function,
                    key,
                    Some(value),
                    &value.ty,
                    value.span,
                    &format!("{path}.value.ty"),
                );
                let choice = view
                    .as_ref()
                    .and_then(|view| self.dyn_concepts.choice(view));
                let unique_valid = choice.is_some_and(|choice| {
                    concrete.as_ref() == Some(choice.concrete())
                        && witness == &mir::WitnessRef::Concrete(choice.witness())
                        && matches!(view, Some(Type::View { mutable: expected, .. }) if expected == *mutable)
                });
                let finite_valid = view
                    .as_ref()
                    .and_then(|view| self.dyn_concepts.finite(view))
                    .and_then(|finite| match witness {
                        mir::WitnessRef::Concrete(witness) => finite.candidate(*witness),
                        mir::WitnessRef::Parameter(_) | mir::WitnessRef::Apply { .. } => None,
                    })
                    .is_some_and(|(_, candidate)| {
                        concrete.as_ref() == Some(candidate.concrete())
                            && matches!(view, Some(Type::View { mutable: expected, .. }) if expected == *mutable)
                    });
                if let Some(writeback) = writeback {
                    let writeback_ty = self.projected_place(
                        function,
                        key,
                        writeback,
                        PlaceUse::InOut,
                        PlaceSite::expression(expression),
                        &format!("{path}.writeback"),
                    );
                    if concrete != writeback_ty {
                        self.expression_item(UnsupportedFeature::View, function, expression, path);
                    }
                }
                if !unique_valid && !finite_valid {
                    self.expression_item(
                        if choice.is_some()
                            || view
                                .as_ref()
                                .is_some_and(|view| self.dyn_concepts.finite(view).is_some())
                        {
                            UnsupportedFeature::View
                        } else {
                            UnsupportedFeature::DynamicWitnessSet
                        },
                        function,
                        expression,
                        path,
                    );
                }
                expression.ty != Type::Never
            }
            ExprKind::ReborrowView { owner, .. } => {
                let owner_ty = self.projected_place(
                    function,
                    key,
                    owner,
                    PlaceUse::Read,
                    PlaceSite::expression(expression),
                    &format!("{path}.owner"),
                );
                let result_ty = self.instantiated_type(
                    function,
                    key,
                    Some(expression),
                    &expression.ty,
                    expression.span,
                    &format!("{path}.ty"),
                );
                let same_choice = owner_ty
                    .as_ref()
                    .zip(result_ty.as_ref())
                    .and_then(|(owner, result)| {
                        self.dyn_concepts
                            .choice(owner)
                            .zip(self.dyn_concepts.choice(result))
                    })
                    .is_some_and(|(owner, result)| owner == result);
                let same_finite = owner_ty
                    .as_ref()
                    .zip(result_ty.as_ref())
                    .and_then(|(owner, result)| {
                        self.dyn_concepts
                            .finite(owner)
                            .zip(self.dyn_concepts.finite(result))
                    })
                    .is_some_and(|(owner, result)| owner == result);
                if !owner.projection.is_empty() || (!same_choice && !same_finite) {
                    self.expression_item(UnsupportedFeature::View, function, expression, path);
                }
                true
            }
            ExprKind::Await { task, .. } => {
                if !self.visit_expr(function, key, task, &format!("{path}.task")) {
                    return false;
                }
                let supported = function.is_async
                    && matches!(
                        self.instantiated_type(
                            function,
                            key,
                            Some(task),
                            &task.ty,
                            task.span,
                            &format!("{path}.task.ty"),
                        ),
                        Some(Type::Task(output)) if output.as_ref() == &expression.ty
                    );
                if !supported {
                    self.expression_item(
                        UnsupportedFeature::Suspension,
                        function,
                        expression,
                        path,
                    );
                }
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
    program: &mir::Program,
    function: &mir::Function,
    key: &InstanceKey,
    calls: &[InstanceKey],
    dyn_concepts: &DynConceptPlan,
) -> InstanceEffectSummary {
    let mut summary = EffectSummary::default();
    if function.is_async {
        summary.include(Effects::NEEDS_EXECUTOR);
    }
    scan_effect_block(program, &function.body, &mut summary);
    let substitution = InstanceSubstitution::new(program, key);
    if function.exprs_preorder().any(|expression| {
        matches!(expression.kind, ExprKind::MakeView { .. })
            && substitution
                .instantiate_type(&expression.ty)
                .ok()
                .is_some_and(|ty| dyn_concepts.finite(&ty).is_some())
    }) {
        summary.include(Effects::MAY_COLLECT);
    }
    if function.exprs_preorder().any(|expression| {
        let ExprKind::Call {
            target: CallTarget::Dynamic { requirement },
            arguments,
            ..
        } = &expression.kind
        else {
            return false;
        };
        program
            .requirement(*requirement)
            .is_some_and(|requirement| requirement.receiver == Some(mir::Receiver::Mutable))
            && arguments.first().is_some_and(|receiver| {
                let receiver_ty = match receiver {
                    CallArgument::Value(receiver) => Some(&receiver.ty),
                    CallArgument::InOut(place) if place.projection.is_empty() => function
                        .params
                        .iter()
                        .chain(&function.locals)
                        .find(|local| local.id == place.local)
                        .map(|local| &local.ty),
                    CallArgument::InOut(_) => None,
                };
                receiver_ty
                    .and_then(|receiver_ty| substitution.instantiate_type(receiver_ty).ok())
                    .is_some_and(|ty| dyn_concepts.finite(&ty).is_some())
            })
    }) {
        summary.include(Effects::MAY_COLLECT);
    }
    // Preconditions execute at each concrete caller boundary. They make the
    // caller's operation fallible, never the assumed callee body by itself.
    if calls.iter().any(|callee| {
        program
            .function(callee.source())
            .is_some_and(|source| !source.call_plan.requires.is_empty())
    }) {
        summary.include(Effects::MAY_FAULT);
    }
    // Entry/current invariants and postconditions belong to the assumed body.
    if function.call_plan.receiver_invariant.is_some() || !function.call_plan.ensures.is_empty() {
        summary.include(Effects::MAY_FAULT);
    }
    InstanceEffectSummary {
        key: key.clone(),
        local: summary.local,
        calls: calls.to_vec().into_boxed_slice(),
    }
}

fn scan_effect_block(
    program: &mir::Program,
    block: &mir::Block,
    summary: &mut EffectSummary,
) -> bool {
    for statement in &block.statements {
        if !scan_effect_statement(program, statement, summary) {
            return false;
        }
    }
    block
        .tail
        .as_deref()
        .is_none_or(|tail| scan_effect_expr(program, tail, summary))
}

fn scan_effect_statement(
    program: &mir::Program,
    statement: &mir::Statement,
    summary: &mut EffectSummary,
) -> bool {
    match &statement.kind {
        StatementKind::Let { value, .. }
        | StatementKind::LetTuple { value, .. }
        | StatementKind::Assign { value, .. }
        | StatementKind::Evaluate(value) => scan_effect_expr(program, value, summary),
        StatementKind::Scoped {
            value, disposal, ..
        } => {
            let continues = scan_effect_expr(program, value, summary);
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
            if !scan_effect_expr(program, start, summary)
                || !scan_effect_expr(program, end, summary)
            {
                return false;
            }
            scan_effect_block(program, body, summary);
            true
        }
        StatementKind::Assert { condition } => {
            let continues = scan_effect_expr(program, condition, summary);
            if continues {
                summary.include(Effects::MAY_FAULT);
            }
            continues
        }
        StatementKind::Defer(cleanup) => {
            scan_effect_block(program, cleanup, summary);
            true
        }
        StatementKind::Return(value) => {
            if let Some(value) = value {
                scan_effect_expr(program, value, summary);
            }
            false
        }
    }
}

#[allow(clippy::too_many_lines)]
fn scan_effect_expr(
    program: &mir::Program,
    expression: &mir::Expr,
    summary: &mut EffectSummary,
) -> bool {
    match &expression.kind {
        ExprKind::Constant(_)
        | ExprKind::Copy(_)
        | ExprKind::Move(_)
        | ExprKind::ReborrowView { .. } => true,
        ExprKind::Tuple(values) => scan_effect_exprs(program, values, summary),
        ExprKind::List(values) => {
            let continues = scan_effect_exprs(program, values, summary);
            if continues && !values.is_empty() {
                summary.include(Effects::MAY_COLLECT);
            }
            continues
        }
        ExprKind::Unary(operator, operand) => {
            if !scan_effect_expr(program, operand, summary) {
                return false;
            }
            if *operator == UnaryOp::Negate && operand.ty == Type::Int {
                summary.include(Effects::MAY_FAULT);
            }
            true
        }
        ExprKind::Binary(operator, left, right) => {
            if !scan_effect_expr(program, left, summary) {
                return false;
            }
            let right_continues = scan_effect_expr(program, right, summary);
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
        ExprKind::Block(block) => scan_effect_block(program, block, summary),
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            scan_effect_expr(program, condition, summary)
                && (scan_effect_block(program, then_branch, summary)
                    | scan_effect_block(program, else_branch, summary))
        }
        ExprKind::Match { scrutinee, arms } => {
            if !scan_effect_expr(program, scrutinee, summary) {
                return false;
            }
            arms.iter().fold(false, |continues, arm| {
                scan_effect_expr(program, &arm.value, summary) | continues
            })
        }
        ExprKind::Record {
            ty,
            fields,
            construction,
            ..
        } => {
            let continues = scan_effect_exprs(program, fields, summary);
            if continues
                && (*construction == mir::ConstructionMode::Recheck
                    || (*construction == mir::ConstructionMode::Runtime
                        && program.type_def(*ty).is_some_and(|definition| {
                            let mir::TypeDefKind::Record {
                                invariant: Some(invariant),
                                ..
                            } = &definition.kind
                            else {
                                return false;
                            };
                            contract_expr_may_fault(
                                program,
                                &invariant.expression,
                                &ContractTypeContext {
                                    receiver: Some(Type::Nominal(*ty, Vec::new())),
                                    result: None,
                                    arguments: Vec::new(),
                                    old_receiver: None,
                                    old_arguments: Vec::new(),
                                    bindings: Vec::new(),
                                },
                            )
                        })))
            {
                summary.include(Effects::MAY_FAULT);
            }
            continues
        }
        ExprKind::Variant { payload, .. } => scan_effect_exprs(program, payload, summary),
        ExprKind::Refine {
            ty,
            value,
            construction,
            ..
        } => {
            let continues = scan_effect_expr(program, value, summary);
            if continues
                && (*construction == mir::ConstructionMode::Recheck
                    || (*construction == mir::ConstructionMode::Runtime
                        && program.type_def(*ty).is_some_and(|definition| {
                            let mir::TypeDefKind::Refined { base, predicate } = &definition.kind
                            else {
                                return false;
                            };
                            contract_expr_may_fault(
                                program,
                                &predicate.expression,
                                &ContractTypeContext {
                                    receiver: Some(base.clone()),
                                    result: None,
                                    arguments: Vec::new(),
                                    old_receiver: None,
                                    old_arguments: Vec::new(),
                                    bindings: Vec::new(),
                                },
                            )
                        })))
            {
                summary.include(Effects::MAY_FAULT);
            }
            continues
        }
        ExprKind::Call {
            target, arguments, ..
        } => {
            for argument in arguments {
                if let CallArgument::Value(value) = argument
                    && !scan_effect_expr(program, value, summary)
                {
                    return false;
                }
            }
            if let CallTarget::Direct(callee) | CallTarget::Inherent(callee) = target {
                summary.calls.insert(*callee);
                if program
                    .function(*callee)
                    .is_some_and(|function| function.is_async)
                {
                    summary.include(Effects::NEEDS_EXECUTOR);
                }
            } else if matches!(
                target,
                CallTarget::Builtin(
                    mir::Builtin::TextConcat
                        | mir::Builtin::TextGet
                        | mir::Builtin::ListAdd
                        | mir::Builtin::TextMapInsert
                        | mir::Builtin::FormatFloat
                )
            ) {
                summary.include(Effects::MAY_COLLECT);
            } else if matches!(
                target,
                CallTarget::Builtin(mir::Builtin::DurationMilliseconds)
            ) {
                summary.include(Effects::MAY_FAULT);
            }
            expression.ty != Type::Never
        }
        ExprKind::Unrefine(value) | ExprKind::MakeView { value, .. } => {
            scan_effect_expr(program, value, summary)
        }
        ExprKind::Await { task: value, .. } => {
            let continues = scan_effect_expr(program, value, summary);
            if continues {
                summary.include(Effects::MAY_SUSPEND);
            }
            continues
        }
        ExprKind::Sleep {
            milliseconds: value,
        } => scan_effect_expr(program, value, summary),
        ExprKind::TaskJoin { arguments, .. } => scan_effect_exprs(program, arguments, summary),
    }
}

fn scan_effect_exprs(
    program: &mir::Program,
    expressions: &[mir::Expr],
    summary: &mut EffectSummary,
) -> bool {
    expressions
        .iter()
        .all(|expression| scan_effect_expr(program, expression, summary))
}

fn continuing_mutations(block: &mir::Block) -> Option<BTreeSet<LocalId>> {
    let mut changed = BTreeSet::new();
    scan_mutation_block(block, &mut changed).then_some(changed)
}

fn canonical_unique_list_loop_body(block: &mir::Block, local: LocalId) -> bool {
    let mut append_count = 0_usize;
    for statement in &block.statements {
        if let StatementKind::Evaluate(expression) = &statement.kind
            && let ExprKind::Call {
                target: CallTarget::Builtin(mir::Builtin::ListAdd),
                arguments,
                ..
            } = &expression.kind
            && let [CallArgument::InOut(receiver), CallArgument::Value(value)] =
                arguments.as_slice()
            && receiver.local == local
            && receiver.projection.is_empty()
            && !expr_mentions_local(value, local)
        {
            append_count = append_count.saturating_add(1);
            continue;
        }
        if statement_mentions_local(statement, local) {
            return false;
        }
    }
    append_count != 0
        && block
            .tail
            .as_deref()
            .is_none_or(|tail| !expr_mentions_local(tail, local))
}

fn statement_mentions_local(statement: &mir::Statement, local: LocalId) -> bool {
    match &statement.kind {
        StatementKind::Let { value, .. }
        | StatementKind::Scoped { value, .. }
        | StatementKind::LetTuple { value, .. }
        | StatementKind::Assert { condition: value }
        | StatementKind::Evaluate(value) => expr_mentions_local(value, local),
        StatementKind::ForRange {
            local: loop_local,
            start,
            end,
            body,
        } => {
            *loop_local == local
                || expr_mentions_local(start, local)
                || expr_mentions_local(end, local)
                || body
                    .statements
                    .iter()
                    .any(|statement| statement_mentions_local(statement, local))
                || body
                    .tail
                    .as_deref()
                    .is_some_and(|tail| expr_mentions_local(tail, local))
        }
        StatementKind::Assign { place, value } => {
            place.local == local || expr_mentions_local(value, local)
        }
        StatementKind::Defer(cleanup) => {
            cleanup
                .statements
                .iter()
                .any(|statement| statement_mentions_local(statement, local))
                || cleanup
                    .tail
                    .as_deref()
                    .is_some_and(|tail| expr_mentions_local(tail, local))
        }
        StatementKind::Return(value) => value
            .as_ref()
            .is_some_and(|value| expr_mentions_local(value, local)),
    }
}

#[allow(clippy::too_many_lines)]
fn expr_mentions_local(expression: &mir::Expr, local: LocalId) -> bool {
    match &expression.kind {
        ExprKind::Constant(_) => false,
        ExprKind::Copy(place) | ExprKind::Move(place) => place.local == local,
        ExprKind::Tuple(values)
        | ExprKind::List(values)
        | ExprKind::TaskJoin {
            arguments: values, ..
        } => values.iter().any(|value| expr_mentions_local(value, local)),
        ExprKind::Unary(_, value)
        | ExprKind::Refine { value, .. }
        | ExprKind::Unrefine(value)
        | ExprKind::Await { task: value, .. }
        | ExprKind::Sleep {
            milliseconds: value,
        } => expr_mentions_local(value, local),
        ExprKind::Binary(_, left, right) => {
            expr_mentions_local(left, local) || expr_mentions_local(right, local)
        }
        ExprKind::Block(block) => {
            block
                .statements
                .iter()
                .any(|statement| statement_mentions_local(statement, local))
                || block
                    .tail
                    .as_deref()
                    .is_some_and(|tail| expr_mentions_local(tail, local))
        }
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            expr_mentions_local(condition, local)
                || then_branch
                    .statements
                    .iter()
                    .any(|statement| statement_mentions_local(statement, local))
                || then_branch
                    .tail
                    .as_deref()
                    .is_some_and(|tail| expr_mentions_local(tail, local))
                || else_branch
                    .statements
                    .iter()
                    .any(|statement| statement_mentions_local(statement, local))
                || else_branch
                    .tail
                    .as_deref()
                    .is_some_and(|tail| expr_mentions_local(tail, local))
        }
        ExprKind::Match { scrutinee, arms } => {
            expr_mentions_local(scrutinee, local)
                || arms
                    .iter()
                    .any(|arm| expr_mentions_local(&arm.value, local))
        }
        ExprKind::Record { fields, .. } => {
            fields.iter().any(|field| expr_mentions_local(field, local))
        }
        ExprKind::Variant { payload, .. } => payload
            .iter()
            .any(|value| expr_mentions_local(value, local)),
        ExprKind::Call { arguments, .. } => arguments.iter().any(|argument| match argument {
            CallArgument::Value(value) => expr_mentions_local(value, local),
            CallArgument::InOut(place) => place.local == local,
        }),
        ExprKind::MakeView {
            value, writeback, ..
        } => {
            expr_mentions_local(value, local)
                || writeback.as_ref().is_some_and(|place| place.local == local)
        }
        ExprKind::ReborrowView { owner, .. } => owner.local == local,
    }
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

#[derive(Clone)]
struct LoweredContractArm {
    source_arm: usize,
    block: BlockId,
    captures: Box<[(LocalId, crate::match_plan::MatchValueId, ValueId)]>,
}

struct InOutArgumentPlan {
    parameter: usize,
    place: PlacePlan,
}

#[derive(Clone, Debug)]
struct ContractOperand {
    value: ValueId,
    ty: Type,
}

#[derive(Clone, Debug)]
struct ContractRecordCandidate {
    ty: Type,
    fields: Vec<ContractOperand>,
}

#[derive(Clone, Debug)]
struct ContractContext {
    receiver: Option<ContractOperand>,
    record_candidate: Option<ContractRecordCandidate>,
    result: Option<ContractOperand>,
    arguments: Vec<ContractOperand>,
    old_receiver: Option<ContractOperand>,
    old_arguments: Vec<Option<ContractOperand>>,
    bindings: Vec<ContractOperand>,
}

struct FunctionLowerer<'function, 'builder, 'plan> {
    program: &'plan mir::Program,
    dyn_concepts: &'plan DynConceptPlan,
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
    old_parameters: Vec<ContractOperand>,
    unique_list_values: BTreeSet<ValueId>,
}

impl<'function, 'builder, 'plan> FunctionLowerer<'function, 'builder, 'plan> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        program: &'plan mir::Program,
        source: &'function mir::Function,
        key: &'plan InstanceKey,
        builder: FunctionBuilder<'builder>,
        instances: &'plan InstanceLookup,
        effects: &'plan [Effects],
        match_plans: &'plan BTreeMap<String, BTreeMap<ExprId, MatchPlan>>,
        dyn_concepts: &'plan DynConceptPlan,
    ) -> Self {
        let local_types = source
            .params
            .iter()
            .chain(&source.locals)
            .map(|local| (local.id, local.ty.clone()))
            .collect();
        let substitution = InstanceSubstitution::new(program, key);
        let inout_locals = source
            .params
            .iter()
            .filter_map(|parameter| {
                substitution
                    .instantiate_type(&parameter.ty)
                    .ok()
                    .is_some_and(|ty| parameter.mutable || is_mutable_view(&ty))
                    .then_some(parameter.id)
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            program,
            dyn_concepts,
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
            old_parameters: Vec::new(),
            unique_list_values: BTreeSet::new(),
        }
    }

    fn is_list_value(&self, value: ValueId) -> bool {
        self.builder
            .value_type(value)
            .and_then(|ty| self.builder.representations().value_type(ty))
            .is_some_and(|ty| matches!(ty.semantic(), Type::List(_)))
    }

    fn share_list_value(&mut self, value: ValueId) {
        if self.is_list_value(value) {
            self.unique_list_values.remove(&value);
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
            let instantiated = InstanceSubstitution::new(self.program, self.key)
                .instantiate_type(&parameter.ty)
                .map_err(|error| instantiation_defect(self.source.id, None, error))?;
            self.old_parameters.push(ContractOperand {
                value,
                ty: instantiated,
            });
        }
        let mut flow = Flow { block: entry, env };
        if self.key.role() == InstanceRole::CheckedRoot {
            return self.lower_checked_root(flow);
        }
        if let Some(contract) = &self.source.call_plan.receiver_invariant {
            let context = self.contract_context(flow.env, None, false)?;
            flow = self.lower_contract_check(
                flow,
                contract,
                ContractFaultKind::Invariant,
                contract.span,
                &context,
            )?;
        }
        match self.lower_scoped_block(flow, &self.source.body)? {
            EvalFlow::Continue { flow, value } => {
                let flow = self.lower_exit_contracts(flow, value)?;
                self.terminate_exit(
                    flow,
                    TerminatorKind::Return(value),
                    self.block_origin(&self.source.body),
                )
            }
            EvalFlow::Terminated => Ok(()),
        }
    }

    fn type_id(&self, ty: &Type) -> Result<ValueTypeId, LoweringError> {
        let instantiated = InstanceSubstitution::new(self.program, self.key)
            .instantiate_type(ty)
            .map_err(|error| instantiation_defect(self.source.id, None, error))?;
        let physical = self.dyn_concepts.physical_type(&instantiated).ok_or_else(|| {
            LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                format!(
                    "classified dynamic type {instantiated:?} has no unique concrete representation"
                ),
            )
        })?;
        self.builder
            .representations()
            .type_id(&physical)
            .ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    format!("classified direct type {physical:?} has no LCIR type"),
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

    fn place_plan(&self, place: &mir::Place, usage: PlaceUse) -> Result<PlacePlan, LoweringError> {
        let root_type = self.local_type(place.local)?;
        let invariant_receiver = self.source.receiver == Some(mir::Receiver::Mutable)
            && self
                .source
                .params
                .first()
                .is_some_and(|receiver| receiver.id == place.local)
            && self
                .local_types
                .get(&place.local)
                .and_then(|ty| {
                    InstanceSubstitution::new(self.program, self.key)
                        .instantiate_type(ty)
                        .ok()
                })
                .is_some_and(|ty| is_invariant_record_type(self.program, &ty));
        let invariant_read = matches!(usage, PlaceUse::Read | PlaceUse::Move)
            && self
                .local_types
                .get(&place.local)
                .and_then(|ty| {
                    InstanceSubstitution::new(self.program, self.key)
                        .instantiate_type(ty)
                        .ok()
                })
                .is_some_and(|ty| is_invariant_record_type(self.program, &ty));
        let planned = if invariant_receiver || invariant_read {
            PlacePlan::build_invariant_receiver(self.builder.representations(), place, root_type)
        } else {
            PlacePlan::build(self.builder.representations(), place, root_type)
        };
        planned.map_err(|error| {
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
        let mut rebuilt = match self.product_insert(
            flow,
            aggregate,
            leaf.field(),
            value,
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
            rebuilt = match self.product_insert(
                flow,
                parent,
                step.field(),
                rebuilt,
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

    fn product_insert(
        &mut self,
        flow: Flow,
        aggregate: ValueId,
        field: u32,
        value: ValueId,
        ty: ValueTypeId,
        origin: Origin,
    ) -> Result<EvalFlow, LoweringError> {
        let invariant_receiver = self
            .builder
            .representations()
            .value_type(ty)
            .is_some_and(|value| value.kind() == crate::ValueTypeKind::InvariantProduct);
        let kind = if invariant_receiver {
            InstructionKind::InvariantReceiverInsert {
                aggregate,
                field,
                value,
            }
        } else {
            InstructionKind::ProductInsert {
                aggregate,
                field,
                value,
            }
        };
        if invariant_receiver {
            self.one_trusted_instruction(flow, kind, ty, origin)
        } else {
            self.one_instruction(flow, kind, ty, origin)
        }
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

    fn contract_origin(&self, expression: &ContractExpr) -> Origin {
        Origin {
            source_function: self.source.id,
            expression: None,
            span: expression.span,
        }
    }

    fn contract_context(
        &self,
        environment: EnvironmentRoot,
        result: Option<ValueId>,
        include_old: bool,
    ) -> Result<ContractContext, LoweringError> {
        let mut parameters = Vec::with_capacity(self.source.params.len());
        for (index, parameter) in self.source.params.iter().enumerate() {
            let value = self
                .environments
                .get(environment, parameter.id)
                .ok_or_else(|| {
                    LoweringError::defect(
                        LoweringDefectCode::InconsistentPlan,
                        format!("contract context lost parameter local #{}", parameter.id.0),
                    )
                })?;
            let ty = self
                .old_parameters
                .get(index)
                .ok_or_else(|| {
                    LoweringError::defect(
                        LoweringDefectCode::InconsistentPlan,
                        "contract parameter type snapshot is missing",
                    )
                })?
                .ty
                .clone();
            parameters.push(ContractOperand { value, ty });
        }
        let (receiver, arguments) = if self.source.receiver.is_some() {
            let (receiver, arguments) = parameters.split_first().ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    "receiver contract has no receiver parameter",
                )
            })?;
            (Some(receiver.clone()), arguments.to_vec())
        } else {
            (None, parameters)
        };
        let (old_receiver, old_arguments) = if include_old {
            if self.source.receiver.is_some() {
                let (receiver, arguments) = self.old_parameters.split_first().ok_or_else(|| {
                    LoweringError::defect(
                        LoweringDefectCode::InconsistentPlan,
                        "receiver old-value snapshot is missing",
                    )
                })?;
                (
                    Some(receiver.clone()),
                    arguments.iter().cloned().map(Some).collect(),
                )
            } else {
                (
                    None,
                    self.old_parameters.iter().cloned().map(Some).collect(),
                )
            }
        } else {
            (None, Vec::new())
        };
        let result = result
            .map(|value| {
                InstanceSubstitution::new(self.program, self.key)
                    .instantiate_type(&self.source.return_ty)
                    .map(|ty| ContractOperand { value, ty })
            })
            .transpose()
            .map_err(|error| instantiation_defect(self.source.id, None, error))?;
        Ok(ContractContext {
            receiver,
            record_candidate: None,
            result,
            arguments,
            old_receiver,
            old_arguments,
            bindings: Vec::new(),
        })
    }

    fn lower_checked_root(&mut self, mut flow: Flow) -> Result<(), LoweringError> {
        if self.source.params.iter().any(|parameter| parameter.mutable) {
            return Err(LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                "checked artifact root unexpectedly has mutable parameters",
            ));
        }
        let context = self.contract_context(flow.env, None, false)?;
        for contract in &self.source.call_plan.requires {
            flow = self.lower_contract_check(
                flow,
                contract,
                ContractFaultKind::Precondition,
                self.source.span,
                &context,
            )?;
        }
        let body_key = self.key.clone().assumed_body();
        let body = self.instances.get(&body_key).ok_or_else(|| {
            LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                format!("checked root has no assumed body instance for {body_key}"),
            )
        })?;
        let arguments = self
            .source
            .params
            .iter()
            .map(|parameter| {
                self.environments
                    .get(flow.env, parameter.id)
                    .ok_or_else(|| {
                        LoweringError::defect(
                            LoweringDefectCode::InconsistentPlan,
                            format!("checked root lost parameter local #{}", parameter.id.0),
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let result_ty = self.type_id(&self.source.return_ty)?;
        let origin = Origin {
            source_function: self.source.id,
            expression: None,
            span: self.source.span,
        };
        let effect = effect_for(self.effects, body)?;
        if effect.contains(Effects::MAY_FAULT) {
            let normal = self.create_block()?;
            let result = self
                .builder
                .append_block_parameter(normal, result_ty)
                .map_err(LoweringError::from)?;
            let fault = self.fault_target(flow)?;
            self.terminate(
                flow.block,
                TerminatorKind::Invoke {
                    callee: body,
                    arguments: arguments.into_boxed_slice(),
                    normal: ResultTarget::new(normal, []),
                    unwind: fault,
                },
                origin,
            )?;
            flow.block = normal;
            self.terminate_exit(flow, TerminatorKind::Return(result), origin)
        } else {
            let results = self
                .builder
                .append_instruction(
                    flow.block,
                    InstructionKind::DirectCall {
                        callee: body,
                        arguments: arguments.into_boxed_slice(),
                    },
                    &[result_ty],
                    origin,
                )
                .map_err(LoweringError::from)?;
            let result = results.first().copied().ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::Builder,
                    "checked-root body call produced no result",
                )
            })?;
            self.terminate_exit(flow, TerminatorKind::Return(result), origin)
        }
    }

    fn lower_exit_contracts(
        &mut self,
        mut flow: Flow,
        result: ValueId,
    ) -> Result<Flow, LoweringError> {
        if self.source.call_plan.receiver_invariant.is_none()
            && self.source.call_plan.ensures.is_empty()
        {
            return Ok(flow);
        }
        // Explicit returns lower the full cleanup suffix while the lexical
        // plan remains available for sibling paths. Postcondition faults must
        // not run that already-consumed suffix a second time.
        let saved_cleanups = std::mem::take(&mut self.cleanups);
        let lowered = (|| {
            let context = self.contract_context(flow.env, Some(result), true)?;
            if let Some(contract) = &self.source.call_plan.receiver_invariant {
                flow = self.lower_contract_check(
                    flow,
                    contract,
                    ContractFaultKind::Invariant,
                    contract.span,
                    &context,
                )?;
            }
            for contract in &self.source.call_plan.ensures {
                flow = self.lower_contract_check(
                    flow,
                    contract,
                    ContractFaultKind::Postcondition,
                    contract.span,
                    &context,
                )?;
            }
            Ok(flow)
        })();
        self.cleanups = saved_cleanups;
        lowered
    }

    fn lower_contract_check(
        &mut self,
        flow: Flow,
        contract: &Contract,
        kind: ContractFaultKind,
        blame_span: Span,
        context: &ContractContext,
    ) -> Result<Flow, LoweringError> {
        let EvalFlow::Continue {
            flow,
            value: condition,
        } = self.lower_contract_expr(flow, &contract.expression, context)?
        else {
            return Err(LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                "checked contract expression unexpectedly terminated",
            ));
        };
        let success = self.create_block()?;
        let fault = self.fault_target(flow)?;
        self.terminate(
            flow.block,
            TerminatorKind::Assert {
                condition,
                metadata: FaultMetadata::contract(ContractFaultMetadata::contract(
                    kind,
                    contract.code.clone(),
                    contract.span,
                    blame_span,
                )),
                success: BlockTarget::new(success, []),
                fault,
            },
            Origin {
                source_function: self.source.id,
                expression: None,
                span: contract.expression.span,
            },
        )?;
        Ok(Flow {
            block: success,
            env: flow.env,
        })
    }

    #[allow(clippy::too_many_lines)]
    fn lower_contract_expr(
        &mut self,
        flow: Flow,
        expression: &ContractExpr,
        context: &ContractContext,
    ) -> Result<EvalFlow, LoweringError> {
        let origin = self.contract_origin(expression);
        match &expression.kind {
            ContractExprKind::Constant(constant) => match constant {
                mir::Constant::Text(value) => self.one_instruction(
                    flow,
                    InstructionKind::TextLiteral {
                        utf8: value.clone().into_boxed_str(),
                    },
                    self.type_id(&Type::Text)?,
                    origin,
                ),
                mir::Constant::Unit => self.constant(flow, Constant::Unit, &Type::Unit, origin),
                mir::Constant::Bool(value) => {
                    self.constant(flow, Constant::Bool(*value), &Type::Bool, origin)
                }
                mir::Constant::Int(value) => {
                    self.constant(flow, Constant::Int(*value), &Type::Int, origin)
                }
                mir::Constant::Float(value) => {
                    self.constant(flow, Constant::float(*value), &Type::Float, origin)
                }
            },
            ContractExprKind::Value(value) => {
                let operand = contract_operand(*value, context).ok_or_else(|| {
                    LoweringError::defect(
                        LoweringDefectCode::InconsistentPlan,
                        format!("contract value {value:?} is unavailable"),
                    )
                })?;
                Ok(EvalFlow::Continue {
                    flow,
                    value: operand.value,
                })
            }
            ContractExprKind::Binding(index) => {
                let value = context
                    .bindings
                    .get(*index as usize)
                    .ok_or_else(|| {
                        LoweringError::defect(
                            LoweringDefectCode::InconsistentPlan,
                            format!("contract binding #{index} is unavailable"),
                        )
                    })?
                    .value;
                Ok(EvalFlow::Continue { flow, value })
            }
            ContractExprKind::Field(owner, field) => {
                if matches!(
                    &owner.kind,
                    ContractExprKind::Value(ContractValue::SelfValue)
                ) && let Some(candidate) = &context.record_candidate
                {
                    let index = usize::try_from(*field).map_err(|_| {
                        LoweringError::defect(
                            LoweringDefectCode::InconsistentPlan,
                            "record invariant field index does not fit usize",
                        )
                    })?;
                    let value = candidate.fields.get(index).ok_or_else(|| {
                        LoweringError::defect(
                            LoweringDefectCode::InconsistentPlan,
                            format!("record invariant references missing field #{field}"),
                        )
                    })?;
                    return Ok(EvalFlow::Continue {
                        flow,
                        value: value.value,
                    });
                }
                let operand = self.lower_contract_operand(flow, owner, context)?;
                let (flow, operand) = self.normalize_contract_operand(operand, origin)?;
                let aggregate = self.type_id(&operand.ty)?;
                let field_ty = self.product_field_type(aggregate, *field)?;
                self.one_instruction(
                    flow,
                    InstructionKind::ProductExtract {
                        aggregate: operand.value,
                        field: *field,
                    },
                    field_ty,
                    origin,
                )
            }
            ContractExprKind::Unary(operator, operand) => {
                let operand = self.lower_contract_operand(flow, operand, context)?;
                let (flow, operand) = self.normalize_contract_operand(operand, origin)?;
                match (operator, &operand.ty) {
                    (UnaryOp::Not, Type::Bool) => self.one_instruction(
                        flow,
                        InstructionKind::BoolNot {
                            value: operand.value,
                        },
                        self.type_id(&Type::Bool)?,
                        origin,
                    ),
                    (UnaryOp::Negate, Type::Float) => self.one_instruction(
                        flow,
                        InstructionKind::FloatNegate {
                            value: operand.value,
                        },
                        self.type_id(&Type::Float)?,
                        origin,
                    ),
                    (UnaryOp::Negate, Type::Int) => {
                        self.lower_checked_negate(flow, operand.value, origin)
                    }
                    _ => Err(self.unsupported_reached("contract unary operation")),
                }
            }
            ContractExprKind::Binary(operator @ (BinaryOp::And | BinaryOp::Or), left, right) => {
                self.lower_contract_short_circuit(flow, *operator, left, right, expression, context)
            }
            ContractExprKind::Binary(operator, left, right) => {
                let left = self.lower_contract_operand(flow, left, context)?;
                let (flow, left) = self.normalize_contract_operand(left, origin)?;
                let right = self.lower_contract_operand(flow, right, context)?;
                let (flow, right) = self.normalize_contract_operand(right, origin)?;
                self.lower_contract_binary(flow, *operator, &left, &right, origin)
            }
            ContractExprKind::IsFinite(value) => {
                let operand = self.lower_contract_operand(flow, value, context)?;
                let (flow, operand) = self.normalize_contract_operand(operand, origin)?;
                if operand.ty != Type::Float {
                    return Err(self.unsupported_reached("contract is_finite operand"));
                }
                self.lower_float_is_finite(flow, operand.value, origin)
            }
            ContractExprKind::Match { scrutinee, arms } => {
                self.lower_contract_match(flow, scrutinee, arms, expression, context)
            }
        }
    }

    fn lower_contract_operand(
        &mut self,
        flow: Flow,
        expression: &ContractExpr,
        context: &ContractContext,
    ) -> Result<(Flow, ContractOperand), LoweringError> {
        let ty = contract_expr_type(self.program, expression, &contract_type_context(context))
            .ok_or_else(|| self.unsupported_reached("contract expression type"))?;
        let ty = InstanceSubstitution::new(self.program, self.key)
            .instantiate_type(&ty)
            .map_err(|error| instantiation_defect(self.source.id, None, error))?;
        let EvalFlow::Continue { flow, value } =
            self.lower_contract_expr(flow, expression, context)?
        else {
            return Err(LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                "contract operand unexpectedly terminated",
            ));
        };
        Ok((flow, ContractOperand { value, ty }))
    }

    fn normalize_contract_operand(
        &mut self,
        (mut flow, mut operand): (Flow, ContractOperand),
        origin: Origin,
    ) -> Result<(Flow, ContractOperand), LoweringError> {
        for _ in 0..64 {
            let Type::Nominal(id, _) = &operand.ty else {
                return Ok((flow, operand));
            };
            let definition = self.program.type_def(*id).ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    format!("contract operand references missing type #{}", id.0),
                )
            })?;
            let mir::TypeDefKind::Refined { .. } = &definition.kind else {
                return Ok((flow, operand));
            };
            let base = concrete_refined_base(self.program, &operand.ty)
                .ok_or_else(|| self.unsupported_reached("open refined contract operand"))?;
            let EvalFlow::Continue {
                flow: next_flow,
                value,
            } = self.one_instruction(
                flow,
                InstructionKind::Unrefine {
                    value: operand.value,
                },
                self.type_id(&base)?,
                origin,
            )?
            else {
                return Err(LoweringError::defect(
                    LoweringDefectCode::Builder,
                    "contract unrefine unexpectedly terminated",
                ));
            };
            flow = next_flow;
            operand = ContractOperand { value, ty: base };
        }
        Err(self.unsupported_reached("deep refined contract operand"))
    }

    fn lower_contract_binary(
        &mut self,
        flow: Flow,
        operator: BinaryOp,
        left: &ContractOperand,
        right: &ContractOperand,
        origin: Origin,
    ) -> Result<EvalFlow, LoweringError> {
        if left.ty != right.ty {
            return Err(self.unsupported_reached("mismatched contract binary operands"));
        }
        if left.ty == Type::Int
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
                _ => unreachable!(),
            };
            return self.lower_checked_binary(flow, op, left.value, right.value, origin);
        }
        if left.ty == Type::Float
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
                _ => unreachable!(),
            };
            return self.one_instruction(
                flow,
                InstructionKind::FloatBinary {
                    op,
                    left: left.value,
                    right: right.value,
                },
                self.type_id(&Type::Float)?,
                origin,
            );
        }
        if matches!(operator, BinaryOp::Equal | BinaryOp::NotEqual)
            && !matches!(left.ty, Type::Bool | Type::Int | Type::Float | Type::Text)
        {
            return self.lower_structural_equality(
                flow,
                operator,
                left.value,
                right.value,
                self.type_id(&left.ty)?,
                origin,
            );
        }
        let kind = match &left.ty {
            Type::Bool => InstructionKind::BoolCompare {
                predicate: match operator {
                    BinaryOp::Equal => BoolPredicate::Equal,
                    BinaryOp::NotEqual => BoolPredicate::NotEqual,
                    _ => return Err(self.unsupported_reached("contract Bool comparison")),
                },
                left: left.value,
                right: right.value,
            },
            Type::Int => InstructionKind::IntCompare {
                predicate: int_predicate(operator)
                    .ok_or_else(|| self.unsupported_reached("contract Int comparison"))?,
                left: left.value,
                right: right.value,
            },
            Type::Float => InstructionKind::FloatCompare {
                predicate: float_predicate(operator)
                    .ok_or_else(|| self.unsupported_reached("contract Float comparison"))?,
                left: left.value,
                right: right.value,
            },
            Type::Text => InstructionKind::TextCompare {
                predicate: match operator {
                    BinaryOp::Equal => BoolPredicate::Equal,
                    BinaryOp::NotEqual => BoolPredicate::NotEqual,
                    _ => return Err(self.unsupported_reached("contract Text comparison")),
                },
                left: left.value,
                right: right.value,
            },
            _ => return Err(self.unsupported_reached("contract aggregate comparison")),
        };
        self.one_instruction(flow, kind, self.type_id(&Type::Bool)?, origin)
    }

    fn lower_contract_short_circuit(
        &mut self,
        flow: Flow,
        operator: BinaryOp,
        left: &ContractExpr,
        right: &ContractExpr,
        expression: &ContractExpr,
        context: &ContractContext,
    ) -> Result<EvalFlow, LoweringError> {
        let EvalFlow::Continue {
            flow,
            value: condition,
        } = self.lower_contract_expr(flow, left, context)?
        else {
            return Err(self.unsupported_reached("terminating contract LHS"));
        };
        let evaluate = self.create_block()?;
        let EvalFlow::Continue {
            flow: right_flow,
            value: right_value,
        } = self.lower_contract_expr(
            Flow {
                block: evaluate,
                env: flow.env,
            },
            right,
            context,
        )?
        else {
            return Err(self.unsupported_reached("terminating contract RHS"));
        };
        let join = self.create_block()?;
        let result = self
            .builder
            .append_block_parameter(join, self.type_id(&Type::Bool)?)
            .map_err(LoweringError::from)?;
        let skip = BlockTarget::new(join, [condition]);
        let evaluate_target = BlockTarget::new(evaluate, []);
        let (then_target, else_target) = match operator {
            BinaryOp::And => (evaluate_target, skip),
            BinaryOp::Or => (skip, evaluate_target),
            _ => return Err(self.unsupported_reached("contract short circuit")),
        };
        let origin = self.contract_origin(expression);
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
            TerminatorKind::Jump(BlockTarget::new(join, [right_value])),
            origin,
        )?;
        Ok(EvalFlow::Continue {
            flow: Flow {
                block: join,
                env: flow.env,
            },
            value: result,
        })
    }

    fn lower_float_is_finite(
        &mut self,
        flow: Flow,
        value: ValueId,
        origin: Origin,
    ) -> Result<EvalFlow, LoweringError> {
        let EvalFlow::Continue {
            flow,
            value: minimum,
        } = self.constant(flow, Constant::float(-f64::MAX), &Type::Float, origin)?
        else {
            return Err(self.unsupported_reached("finite lower constant"));
        };
        let EvalFlow::Continue {
            flow,
            value: maximum,
        } = self.constant(flow, Constant::float(f64::MAX), &Type::Float, origin)?
        else {
            return Err(self.unsupported_reached("finite upper constant"));
        };
        let EvalFlow::Continue { flow, value: lower } = self.one_instruction(
            flow,
            InstructionKind::FloatCompare {
                predicate: FloatPredicate::OrderedGreaterEqual,
                left: value,
                right: minimum,
            },
            self.type_id(&Type::Bool)?,
            origin,
        )?
        else {
            return Err(self.unsupported_reached("finite lower comparison"));
        };
        let EvalFlow::Continue { flow, value: upper } = self.one_instruction(
            flow,
            InstructionKind::FloatCompare {
                predicate: FloatPredicate::OrderedLessEqual,
                left: value,
                right: maximum,
            },
            self.type_id(&Type::Bool)?,
            origin,
        )?
        else {
            return Err(self.unsupported_reached("finite upper comparison"));
        };
        let join = self.create_block()?;
        let result = self
            .builder
            .append_block_parameter(join, self.type_id(&Type::Bool)?)
            .map_err(LoweringError::from)?;
        self.terminate(
            flow.block,
            TerminatorKind::Branch {
                condition: lower,
                then_target: BlockTarget::new(join, [upper]),
                else_target: BlockTarget::new(join, [lower]),
            },
            origin,
        )?;
        Ok(EvalFlow::Continue {
            flow: Flow {
                block: join,
                env: flow.env,
            },
            value: result,
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "contract match lowering keeps the shared decision plan, typed captures, and arm join construction in one auditable boundary"
    )]
    fn lower_contract_match(
        &mut self,
        flow: Flow,
        scrutinee: &ContractExpr,
        arms: &[mir::ContractArm],
        expression: &ContractExpr,
        context: &ContractContext,
    ) -> Result<EvalFlow, LoweringError> {
        let scrutinee = self.lower_contract_operand(flow, scrutinee, context)?;
        let (flow, scrutinee) =
            self.normalize_contract_operand(scrutinee, self.contract_origin(expression))?;
        let plan = plan_contract_match(self.program, &scrutinee.ty, arms, context.bindings.len())
            .ok_or_else(|| self.unsupported_reached("unplanned contract match"))?;
        let mut values = vec![None; plan.value_count()];
        values[0] = Some(scrutinee.value);
        let mut lowered_arms = BTreeMap::new();
        for (node, decision) in plan.nodes() {
            let MatchNode::Arm { arm, captures } = decision else {
                continue;
            };
            let block = self.create_block()?;
            let mut parameters = Vec::with_capacity(captures.len());
            for (binding, capture) in captures.iter().copied() {
                let ty = plan.value_type(capture).ok_or_else(|| {
                    LoweringError::defect(
                        LoweringDefectCode::InconsistentPlan,
                        "contract match capture has no planned type",
                    )
                })?;
                let parameter = self
                    .builder
                    .append_block_parameter(block, self.type_id(ty)?)
                    .map_err(LoweringError::from)?;
                parameters.push((binding, capture, parameter));
            }
            lowered_arms.insert(
                node,
                LoweredContractArm {
                    source_arm: *arm,
                    block,
                    captures: parameters.into_boxed_slice(),
                },
            );
        }
        self.lower_contract_match_node(
            &plan,
            plan.root(),
            flow,
            &values,
            expression,
            &lowered_arms,
        )?;
        let result_ty =
            contract_expr_type(self.program, expression, &contract_type_context(context))
                .ok_or_else(|| self.unsupported_reached("contract match result type"))?;
        let result_ty = InstanceSubstitution::new(self.program, self.key)
            .instantiate_type(&result_ty)
            .map_err(|error| instantiation_defect(self.source.id, None, error))?;
        let join = self.create_block()?;
        let result = self
            .builder
            .append_block_parameter(join, self.type_id(&result_ty)?)
            .map_err(LoweringError::from)?;
        for lowered in lowered_arms.values().cloned() {
            let arm = arms.get(lowered.source_arm).ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    "contract match plan references a missing arm",
                )
            })?;
            let mut nested = context.clone();
            let binding_base = nested.bindings.len();
            let mut captures = lowered.captures.to_vec();
            captures.sort_unstable_by_key(|(binding, _, _)| binding.0);
            for (offset, declared) in arm.bindings.iter().enumerate() {
                let expected =
                    u32::try_from(binding_base.checked_add(offset).ok_or_else(|| {
                        LoweringError::defect(
                            LoweringDefectCode::InconsistentPlan,
                            "contract binding index overflowed",
                        )
                    })?)
                    .map(LocalId)
                    .map_err(|_| {
                        LoweringError::defect(
                            LoweringDefectCode::InconsistentPlan,
                            "contract binding index exceeded u32",
                        )
                    })?;
                let (_, _, value) = captures
                    .iter()
                    .find(|(binding, _, _)| *binding == expected)
                    .copied()
                    .ok_or_else(|| {
                        LoweringError::defect(
                            LoweringDefectCode::InconsistentPlan,
                            "contract match capture is missing",
                        )
                    })?;
                let ty = InstanceSubstitution::new(self.program, self.key)
                    .instantiate_type(declared)
                    .map_err(|error| instantiation_defect(self.source.id, None, error))?;
                nested.bindings.push(ContractOperand { value, ty });
            }
            let EvalFlow::Continue {
                flow: arm_flow,
                value,
            } = self.lower_contract_expr(
                Flow {
                    block: lowered.block,
                    env: flow.env,
                },
                &arm.value,
                &nested,
            )?
            else {
                return Err(self.unsupported_reached("terminating contract match arm"));
            };
            self.terminate(
                arm_flow.block,
                TerminatorKind::Jump(BlockTarget::new(join, [value])),
                self.contract_origin(&arm.value),
            )?;
        }
        Ok(EvalFlow::Continue {
            flow: Flow {
                block: join,
                env: flow.env,
            },
            value: result,
        })
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn lower_contract_match_node(
        &mut self,
        plan: &MatchPlan,
        node: crate::match_plan::MatchNodeId,
        flow: Flow,
        values: &[Option<ValueId>],
        expression: &ContractExpr,
        lowered_arms: &BTreeMap<crate::match_plan::MatchNodeId, LoweredContractArm>,
    ) -> Result<(), LoweringError> {
        let decision = plan.node(node).cloned().ok_or_else(|| {
            LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                "contract match decision references a missing node",
            )
        })?;
        let origin = self.contract_origin(expression);
        match decision {
            MatchNode::Arm { arm, captures } => {
                let lowered = lowered_arms.get(&node).ok_or_else(|| {
                    LoweringError::defect(
                        LoweringDefectCode::InconsistentPlan,
                        format!("contract match has no block for arm {arm}"),
                    )
                })?;
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
                                    "contract match capture value is unavailable",
                                )
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                self.terminate(
                    flow.block,
                    TerminatorKind::Jump(BlockTarget::new(lowered.block, arguments)),
                    origin,
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
                            "contract match constant operand is unavailable",
                        )
                    })?;
                let ty = plan.value_type(value).ok_or_else(|| {
                    LoweringError::defect(
                        LoweringDefectCode::InconsistentPlan,
                        "contract match constant has no type",
                    )
                })?;
                let expected = match constant {
                    mir::Constant::Text(text) => {
                        let EvalFlow::Continue { flow: next, value } = self.one_instruction(
                            flow,
                            InstructionKind::TextLiteral {
                                utf8: text.into_boxed_str(),
                            },
                            self.type_id(&Type::Text)?,
                            origin,
                        )?
                        else {
                            return Err(self.unsupported_reached("contract Text pattern"));
                        };
                        return self.lower_contract_match_constant_branch(
                            plan,
                            equal,
                            not_equal,
                            next,
                            values,
                            expression,
                            lowered_arms,
                            InstructionKind::TextCompare {
                                predicate: BoolPredicate::Equal,
                                left: operand,
                                right: value,
                            },
                        );
                    }
                    mir::Constant::Unit => Constant::Unit,
                    mir::Constant::Bool(value) => Constant::Bool(value),
                    mir::Constant::Int(value) => Constant::Int(value),
                    mir::Constant::Float(value) => Constant::float(value),
                };
                let EvalFlow::Continue {
                    flow,
                    value: expected,
                } = self.constant(flow, expected, ty, origin)?
                else {
                    return Err(self.unsupported_reached("contract match constant"));
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
                    Type::Unit => {
                        return self.lower_contract_match_node(
                            plan,
                            equal,
                            flow,
                            values,
                            expression,
                            lowered_arms,
                        );
                    }
                    _ => return Err(self.unsupported_reached("contract constant pattern type")),
                };
                self.lower_contract_match_constant_branch(
                    plan,
                    equal,
                    not_equal,
                    flow,
                    values,
                    expression,
                    lowered_arms,
                    instruction,
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
                            "contract sum scrutinee is unavailable",
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
                                "contract sum payload has no type",
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
                    origin,
                )?;
                for (next, block, case_values) in case_flows {
                    self.lower_contract_match_node(
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

    #[allow(clippy::too_many_arguments)]
    fn lower_contract_match_constant_branch(
        &mut self,
        plan: &MatchPlan,
        equal: crate::match_plan::MatchNodeId,
        not_equal: crate::match_plan::MatchNodeId,
        flow: Flow,
        values: &[Option<ValueId>],
        expression: &ContractExpr,
        lowered_arms: &BTreeMap<crate::match_plan::MatchNodeId, LoweredContractArm>,
        instruction: InstructionKind,
    ) -> Result<(), LoweringError> {
        let origin = self.contract_origin(expression);
        let EvalFlow::Continue {
            flow,
            value: condition,
        } = self.one_instruction(flow, instruction, self.type_id(&Type::Bool)?, origin)?
        else {
            return Err(self.unsupported_reached("contract pattern comparison"));
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
            origin,
        )?;
        self.lower_contract_match_node(
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
        self.lower_contract_match_node(
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
        match &kind {
            InstructionKind::ListLength { .. } | InstructionKind::ListGet { .. } => {}
            InstructionKind::ListAppend { value, .. } => self.share_list_value(*value),
            InstructionKind::ListConstruct { elements } => {
                for element in elements.iter().copied() {
                    self.share_list_value(element);
                }
            }
            _ => {
                for operand in kind.operands() {
                    self.share_list_value(operand);
                }
            }
        }
        let establishes_unique_list = matches!(
            &kind,
            InstructionKind::ListConstruct { .. } | InstructionKind::ListAppend { .. }
        );
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
        if establishes_unique_list {
            self.unique_list_values.insert(value);
        }
        Ok(EvalFlow::Continue { flow, value })
    }

    fn one_trusted_instruction(
        &mut self,
        flow: Flow,
        kind: InstructionKind,
        ty: ValueTypeId,
        origin: Origin,
    ) -> Result<EvalFlow, LoweringError> {
        let unique_append = if let InstructionKind::ListAppendUnique { list, value } = &kind {
            self.share_list_value(*value);
            Some(*list)
        } else {
            for operand in kind.operands() {
                self.share_list_value(operand);
            }
            None
        };
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
        if let Some(list) = unique_append {
            self.unique_list_values.remove(&list);
            self.unique_list_values.insert(value);
        }
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

    fn required_instruction(
        &mut self,
        flow: Flow,
        kind: InstructionKind,
        ty: &Type,
        origin: Origin,
    ) -> Result<(Flow, ValueId), LoweringError> {
        match self.one_instruction(flow, kind, self.type_id(ty)?, origin)? {
            EvalFlow::Continue { flow, value } => Ok((flow, value)),
            EvalFlow::Terminated => Err(LoweringError::defect(
                LoweringDefectCode::Builder,
                "one-result LCIR instruction unexpectedly terminated",
            )),
        }
    }

    fn required_trusted_instruction(
        &mut self,
        flow: Flow,
        kind: InstructionKind,
        ty: &Type,
        origin: Origin,
    ) -> Result<(Flow, ValueId), LoweringError> {
        match self.one_trusted_instruction(flow, kind, self.type_id(ty)?, origin)? {
            EvalFlow::Continue { flow, value } => Ok((flow, value)),
            EvalFlow::Terminated => Err(LoweringError::defect(
                LoweringDefectCode::Builder,
                "one-result trusted LCIR instruction unexpectedly terminated",
            )),
        }
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
        let place = self.place_plan(&mir::Place::local(local), PlaceUse::Read)?;
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
        let place = self.place_plan(&mir::Place::local(local), PlaceUse::InOut)?;
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
                    let plan = self.place_plan(place, PlaceUse::Write)?;
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
                            metadata: FaultMetadata::contract(ContractFaultMetadata::assertion(
                                statement.span,
                            )),
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
                        let flow = self.lower_exit_contracts(flow, value)?;
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
                let usage = if matches!(expression.kind, ExprKind::Move(_)) {
                    PlaceUse::Move
                } else {
                    PlaceUse::Read
                };
                let plan = self.place_plan(place, usage)?;
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
                if matches!(expression.kind, ExprKind::Copy(_)) {
                    self.share_list_value(value);
                } else {
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
            ExprKind::List(values) => {
                let mut flow = flow;
                let mut elements = Vec::with_capacity(values.len());
                for value in values {
                    let EvalFlow::Continue {
                        flow: next_flow,
                        value,
                    } = self.lower_expr(flow, value)?
                    else {
                        return Ok(EvalFlow::Terminated);
                    };
                    flow = next_flow;
                    elements.push(value);
                }
                self.one_instruction(
                    flow,
                    InstructionKind::ListConstruct {
                        elements: elements.into_boxed_slice(),
                    },
                    self.type_id(&expression.ty)?,
                    origin,
                )
            }
            ExprKind::Match { scrutinee, arms } => {
                self.lower_match(flow, scrutinee, arms, expression)
            }
            ExprKind::Record {
                ty,
                fields,
                construction,
                ..
            } => {
                let instruction = match construction {
                    mir::ConstructionMode::Plain => ProductConstruction::Plain,
                    mir::ConstructionMode::Proven => ProductConstruction::InvariantProven,
                    mir::ConstructionMode::Runtime => {
                        return self.lower_runtime_checked_record(flow, *ty, fields, expression);
                    }
                    mir::ConstructionMode::Recheck => {
                        return self.lower_rechecked_record(flow, *ty, fields, expression);
                    }
                };
                self.lower_product_values(flow, fields, expression, instruction)
            }
            ExprKind::Variant {
                variant, payload, ..
            } => self.lower_sum_variant(flow, *variant, payload, expression),
            ExprKind::Refine {
                ty,
                value,
                construction,
                ..
            } => {
                match construction {
                    mir::ConstructionMode::Proven => {}
                    mir::ConstructionMode::Runtime => {
                        return self.lower_runtime_checked_refinement(flow, *ty, value, expression);
                    }
                    mir::ConstructionMode::Recheck => {
                        return self.lower_rechecked_refinement(flow, *ty, value, expression);
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
            ExprKind::MakeView { value, witness, .. } => {
                let instantiated = InstanceSubstitution::new(self.program, self.key)
                    .instantiate_type(&expression.ty)
                    .map_err(|error| {
                        instantiation_defect(self.source.id, Some(expression.id), error)
                    })?;
                let EvalFlow::Continue { flow, value } = self.lower_expr(flow, value)? else {
                    return Ok(EvalFlow::Terminated);
                };
                if let Some(choice) = self.dyn_concepts.choice(&instantiated) {
                    if witness != &mir::WitnessRef::Concrete(choice.witness()) {
                        return Err(self.unsupported_reached("dynamic witness choice mismatch"));
                    }
                    if self.builder.value_type(value) != Some(self.type_id(&expression.ty)?) {
                        return Err(LoweringError::defect(
                            LoweringDefectCode::InconsistentPlan,
                            "devirtualized view value does not use its selected concrete type",
                        ));
                    }
                    return Ok(EvalFlow::Continue { flow, value });
                }
                let finite = self
                    .dyn_concepts
                    .finite(&instantiated)
                    .ok_or_else(|| self.unsupported_reached("open dynamic witness set"))?;
                let mir::WitnessRef::Concrete(witness) = witness else {
                    return Err(self.unsupported_reached("non-concrete dynamic witness"));
                };
                let (variant, candidate) = finite
                    .candidate(*witness)
                    .ok_or_else(|| self.unsupported_reached("dynamic witness choice mismatch"))?;
                if self.builder.value_type(value) != Some(self.type_id(candidate.concrete())?) {
                    return Err(LoweringError::defect(
                        LoweringDefectCode::InconsistentPlan,
                        "managed dynamic payload does not use its selected concrete type",
                    ));
                }
                self.one_instruction(
                    flow,
                    InstructionKind::DynConstruct { variant, value },
                    self.type_id(&expression.ty)?,
                    self.expression_origin(expression),
                )
            }
            ExprKind::ReborrowView { owner, .. } => {
                let plan = self.place_plan(owner, PlaceUse::Read)?;
                if plan.leaf_type() != self.type_id(&expression.ty)? {
                    return Err(LoweringError::defect(
                        LoweringDefectCode::InconsistentPlan,
                        "devirtualized reborrow does not preserve its selected concrete type",
                    ));
                }
                self.read_place(flow, &plan, origin)
            }
            ExprKind::Await { state, task } => {
                let EvalFlow::Continue { flow, value: task } = self.lower_expr(flow, task)? else {
                    return Ok(EvalFlow::Terminated);
                };
                let suspension = self
                    .source
                    .suspension_points
                    .iter()
                    .find(|point| point.state == *state)
                    .ok_or_else(|| {
                        LoweringError::defect(
                            LoweringDefectCode::InconsistentPlan,
                            format!(
                                "async function #{} has no suspension metadata for state {}",
                                self.source.id.0, state
                            ),
                        )
                    })?;
                let normal = self.create_block()?;
                let result = self
                    .builder
                    .append_block_parameter(normal, self.type_id(&expression.ty)?)
                    .map_err(LoweringError::from)?;
                let mut arguments = Vec::with_capacity(suspension.live_locals.len());
                let mut env = EMPTY_ENVIRONMENT;
                for local in &suspension.live_locals {
                    let value = self.environments.get(flow.env, *local).ok_or_else(|| {
                        LoweringError::defect(
                            LoweringDefectCode::InconsistentPlan,
                            format!(
                                "async function #{} suspension state {} lost live local #{}",
                                self.source.id.0, state, local.0
                            ),
                        )
                    })?;
                    arguments.push(value);
                    let parameter = self
                        .builder
                        .append_block_parameter(normal, self.local_type(*local)?)
                        .map_err(LoweringError::from)?;
                    env = self.environments.set(env, *local, parameter)?;
                }
                self.terminate(
                    flow.block,
                    TerminatorKind::AwaitTask {
                        state: *state,
                        task,
                        normal: ResultTarget::new(normal, arguments),
                    },
                    origin,
                )?;
                Ok(EvalFlow::Continue {
                    flow: Flow { block: normal, env },
                    value: result,
                })
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

    #[expect(
        clippy::too_many_lines,
        reason = "accepted and disclosure-safe rejected record construction share one typed control-flow boundary"
    )]
    fn lower_runtime_checked_record(
        &mut self,
        mut flow: Flow,
        ty: mir::TypeId,
        fields: &[mir::Expr],
        expression: &mir::Expr,
    ) -> Result<EvalFlow, LoweringError> {
        let (name, field_types, invariant) = self
            .program
            .type_def(ty)
            .and_then(|definition| (definition.type_parameters == 0).then_some(definition))
            .and_then(|definition| match &definition.kind {
                mir::TypeDefKind::Record {
                    fields,
                    invariant: Some(invariant),
                } => Some((
                    definition.name.clone(),
                    fields
                        .iter()
                        .map(|field| field.ty.clone())
                        .collect::<Vec<_>>(),
                    invariant.clone(),
                )),
                _ => None,
            })
            .ok_or_else(|| self.unsupported_reached("runtime record constraint"))?;
        if field_types.len() != fields.len() {
            return Err(LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                "runtime record constraint field arity changed after classification",
            ));
        }
        let mut lowered = Vec::with_capacity(fields.len());
        let mut candidates = Vec::with_capacity(fields.len());
        for (field, field_ty) in fields.iter().zip(field_types) {
            let EvalFlow::Continue {
                flow: next_flow,
                value,
            } = self.lower_expr(flow, field)?
            else {
                return Ok(EvalFlow::Terminated);
            };
            flow = next_flow;
            lowered.push(value);
            candidates.push(ContractOperand {
                value,
                ty: field_ty,
            });
        }
        let target = Type::Nominal(ty, Vec::new());
        let context = ContractContext {
            receiver: None,
            record_candidate: Some(ContractRecordCandidate {
                ty: target.clone(),
                fields: candidates,
            }),
            result: None,
            arguments: Vec::new(),
            old_receiver: None,
            old_arguments: Vec::new(),
            bindings: Vec::new(),
        };
        let EvalFlow::Continue {
            flow,
            value: condition,
        } = self.lower_contract_expr(flow, &invariant.expression, &context)?
        else {
            return Err(LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                "runtime record predicate unexpectedly terminated",
            ));
        };
        let accepted = self.create_block()?;
        let rejected = self.create_block()?;
        let origin = self.expression_origin(expression);
        self.terminate(
            flow.block,
            TerminatorKind::Branch {
                condition,
                then_target: BlockTarget::new(accepted, []),
                else_target: BlockTarget::new(rejected, []),
            },
            origin,
        )?;
        let (accepted_flow, established) = self.required_trusted_instruction(
            Flow {
                block: accepted,
                env: flow.env,
            },
            InstructionKind::InvariantRecordProven {
                fields: lowered.into_boxed_slice(),
            },
            &target,
            origin,
        )?;
        let (accepted_flow, ok) = self.required_instruction(
            accepted_flow,
            InstructionKind::SumConstruct {
                variant: 0,
                payload: Box::new([established]),
            },
            &expression.ty,
            origin,
        )?;
        let (rejected_flow, error) = self.lower_constraint_error(
            Flow {
                block: rejected,
                env: flow.env,
            },
            &name,
            "InvariantViolation",
            &invariant,
            &target,
            origin,
        )?;
        let (rejected_flow, error) = self.required_instruction(
            rejected_flow,
            InstructionKind::SumConstruct {
                variant: 1,
                payload: Box::new([error]),
            },
            &expression.ty,
            origin,
        )?;
        self.merge_evaluations(
            [
                EvalFlow::Continue {
                    flow: accepted_flow,
                    value: ok,
                },
                EvalFlow::Continue {
                    flow: rejected_flow,
                    value: error,
                },
            ],
            flow.env,
            &expression.ty,
            origin,
        )
    }

    #[expect(
        clippy::too_many_lines,
        reason = "accepted and disclosure-safe rejected refinement construction share one typed control-flow boundary"
    )]
    fn lower_runtime_checked_refinement(
        &mut self,
        flow: Flow,
        ty: mir::TypeId,
        value: &mir::Expr,
        expression: &mir::Expr,
    ) -> Result<EvalFlow, LoweringError> {
        let (name, base, predicate) = self
            .program
            .type_def(ty)
            .and_then(|definition| (definition.type_parameters == 0).then_some(definition))
            .and_then(|definition| match &definition.kind {
                mir::TypeDefKind::Refined { base, predicate } => {
                    Some((definition.name.clone(), base.clone(), predicate.clone()))
                }
                _ => None,
            })
            .ok_or_else(|| self.unsupported_reached("runtime refinement constraint"))?;
        let EvalFlow::Continue { flow, value } = self.lower_expr(flow, value)? else {
            return Ok(EvalFlow::Terminated);
        };
        let context = ContractContext {
            receiver: Some(ContractOperand {
                value,
                ty: base.clone(),
            }),
            record_candidate: None,
            result: None,
            arguments: Vec::new(),
            old_receiver: None,
            old_arguments: Vec::new(),
            bindings: Vec::new(),
        };
        let EvalFlow::Continue {
            flow,
            value: condition,
        } = self.lower_contract_expr(flow, &predicate.expression, &context)?
        else {
            return Err(LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                "runtime refinement predicate unexpectedly terminated",
            ));
        };
        let accepted = self.create_block()?;
        let rejected = self.create_block()?;
        let origin = self.expression_origin(expression);
        self.terminate(
            flow.block,
            TerminatorKind::Branch {
                condition,
                then_target: BlockTarget::new(accepted, []),
                else_target: BlockTarget::new(rejected, []),
            },
            origin,
        )?;
        let target = Type::Nominal(ty, Vec::new());
        let (accepted_flow, established) = self.required_trusted_instruction(
            Flow {
                block: accepted,
                env: flow.env,
            },
            InstructionKind::RefineProven { value },
            &target,
            origin,
        )?;
        let (accepted_flow, ok) = self.required_instruction(
            accepted_flow,
            InstructionKind::SumConstruct {
                variant: 0,
                payload: Box::new([established]),
            },
            &expression.ty,
            origin,
        )?;
        let (rejected_flow, error) = self.lower_constraint_error(
            Flow {
                block: rejected,
                env: flow.env,
            },
            &name,
            "ConstraintViolation",
            &predicate,
            &base,
            origin,
        )?;
        let (rejected_flow, error) = self.required_instruction(
            rejected_flow,
            InstructionKind::SumConstruct {
                variant: 1,
                payload: Box::new([error]),
            },
            &expression.ty,
            origin,
        )?;
        self.merge_evaluations(
            [
                EvalFlow::Continue {
                    flow: accepted_flow,
                    value: ok,
                },
                EvalFlow::Continue {
                    flow: rejected_flow,
                    value: error,
                },
            ],
            flow.env,
            &expression.ty,
            origin,
        )
    }

    fn lower_constraint_error(
        &mut self,
        flow: Flow,
        target_name: &str,
        violation_code: &str,
        contract: &Contract,
        summary_type: &Type,
        origin: Origin,
    ) -> Result<(Flow, ValueId), LoweringError> {
        let summary = disclosure_type_summary(self.program, summary_type);
        let (flow, target) = self.required_instruction(
            flow,
            InstructionKind::TextLiteral {
                utf8: target_name.into(),
            },
            &Type::Text,
            origin,
        )?;
        let (flow, code) = self.required_instruction(
            flow,
            InstructionKind::TextLiteral {
                utf8: violation_code.into(),
            },
            &Type::Text,
            origin,
        )?;
        let (flow, predicate) = self.required_instruction(
            flow,
            InstructionKind::TextLiteral {
                utf8: contract.code.clone().into_boxed_str(),
            },
            &Type::Text,
            origin,
        )?;
        let list_text = Type::List(Box::new(Type::Text));
        let (flow, path) = self.required_instruction(
            flow,
            InstructionKind::ListConstruct {
                elements: Box::new([]),
            },
            &list_text,
            origin,
        )?;
        let (mut flow, summary) = self.required_instruction(
            flow,
            InstructionKind::TextLiteral {
                utf8: summary.into_boxed_str(),
            },
            &Type::Text,
            origin,
        )?;
        let mut span_fields = Vec::with_capacity(3);
        for component in [
            i64::from(contract.span.file.0),
            i64::from(contract.span.range.start),
            i64::from(contract.span.range.end),
        ] {
            let (next_flow, component) = self.required_instruction(
                flow,
                InstructionKind::Constant(Constant::Int(component)),
                &Type::Int,
                origin,
            )?;
            flow = next_flow;
            span_fields.push(component);
        }
        let span_type = Type::Tuple(vec![Type::Int, Type::Int, Type::Int]);
        let (flow, contract_span) = self.required_instruction(
            flow,
            InstructionKind::ProductConstruct {
                fields: span_fields.into_boxed_slice(),
            },
            &span_type,
            origin,
        )?;
        let constraint_error = self
            .program
            .prelude
            .constraint_error
            .map(|ty| Type::Nominal(ty, Vec::new()))
            .ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    "runtime constraint construction has no ConstraintError prelude type",
                )
            })?;
        self.required_instruction(
            flow,
            InstructionKind::ProductConstruct {
                fields: Box::new([target, code, predicate, path, summary, contract_span]),
            },
            &constraint_error,
            origin,
        )
    }

    fn lower_rechecked_record(
        &mut self,
        mut flow: Flow,
        ty: mir::TypeId,
        fields: &[mir::Expr],
        expression: &mir::Expr,
    ) -> Result<EvalFlow, LoweringError> {
        let (field_types, invariant) = self
            .program
            .type_def(ty)
            .and_then(|definition| (definition.type_parameters == 0).then_some(definition))
            .and_then(|definition| match &definition.kind {
                mir::TypeDefKind::Record {
                    fields,
                    invariant: Some(invariant),
                } => Some((
                    fields
                        .iter()
                        .map(|field| field.ty.clone())
                        .collect::<Vec<_>>(),
                    invariant.clone(),
                )),
                _ => None,
            })
            .ok_or_else(|| self.unsupported_reached("serialized record proof recheck"))?;
        if field_types.len() != fields.len() {
            return Err(LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                "record proof recheck field arity changed after classification",
            ));
        }
        let mut lowered = Vec::with_capacity(fields.len());
        let mut candidates = Vec::with_capacity(fields.len());
        for (field, field_ty) in fields.iter().zip(field_types) {
            let EvalFlow::Continue {
                flow: next_flow,
                value,
            } = self.lower_expr(flow, field)?
            else {
                return Ok(EvalFlow::Terminated);
            };
            flow = next_flow;
            let field_ty = InstanceSubstitution::new(self.program, self.key)
                .instantiate_type(&field_ty)
                .map_err(|error| {
                    instantiation_defect(self.source.id, Some(expression.id), error)
                })?;
            lowered.push(value);
            candidates.push(ContractOperand {
                value,
                ty: field_ty,
            });
        }
        let context = ContractContext {
            receiver: None,
            record_candidate: Some(ContractRecordCandidate {
                ty: Type::Nominal(ty, Vec::new()),
                fields: candidates,
            }),
            result: None,
            arguments: Vec::new(),
            old_receiver: None,
            old_arguments: Vec::new(),
            bindings: Vec::new(),
        };
        let EvalFlow::Continue {
            flow,
            value: condition,
        } = self.lower_contract_expr(flow, &invariant.expression, &context)?
        else {
            return Err(LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                "record proof predicate unexpectedly terminated",
            ));
        };
        let flow = self.lower_runtime_assert(
            flow,
            condition,
            FaultCode::ArtifactProofRejected,
            self.expression_origin(expression),
        )?;
        self.one_trusted_instruction(
            flow,
            InstructionKind::InvariantRecordProven {
                fields: lowered.into_boxed_slice(),
            },
            self.type_id(&expression.ty)?,
            self.expression_origin(expression),
        )
    }

    fn lower_rechecked_refinement(
        &mut self,
        flow: Flow,
        ty: mir::TypeId,
        value: &mir::Expr,
        expression: &mir::Expr,
    ) -> Result<EvalFlow, LoweringError> {
        let (base, predicate) = self
            .program
            .type_def(ty)
            .and_then(|definition| (definition.type_parameters == 0).then_some(definition))
            .and_then(|definition| match &definition.kind {
                mir::TypeDefKind::Refined { base, predicate } => {
                    Some((base.clone(), predicate.clone()))
                }
                _ => None,
            })
            .ok_or_else(|| self.unsupported_reached("serialized refinement proof recheck"))?;
        let base = InstanceSubstitution::new(self.program, self.key)
            .instantiate_type(&base)
            .map_err(|error| instantiation_defect(self.source.id, Some(expression.id), error))?;
        let EvalFlow::Continue { flow, value } = self.lower_expr(flow, value)? else {
            return Ok(EvalFlow::Terminated);
        };
        let context = ContractContext {
            receiver: Some(ContractOperand { value, ty: base }),
            record_candidate: None,
            result: None,
            arguments: Vec::new(),
            old_receiver: None,
            old_arguments: Vec::new(),
            bindings: Vec::new(),
        };
        let EvalFlow::Continue {
            flow,
            value: condition,
        } = self.lower_contract_expr(flow, &predicate.expression, &context)?
        else {
            return Err(LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                "refinement proof predicate unexpectedly terminated",
            ));
        };
        let flow = self.lower_runtime_assert(
            flow,
            condition,
            FaultCode::ArtifactProofRejected,
            self.expression_origin(expression),
        )?;
        self.one_trusted_instruction(
            flow,
            InstructionKind::RefineProven { value },
            self.type_id(&expression.ty)?,
            self.expression_origin(expression),
        )
    }

    fn lower_runtime_assert(
        &mut self,
        flow: Flow,
        condition: ValueId,
        code: FaultCode,
        origin: Origin,
    ) -> Result<Flow, LoweringError> {
        let success = self.create_block()?;
        let fault = self.fault_target(flow)?;
        self.terminate(
            flow.block,
            TerminatorKind::Assert {
                condition,
                metadata: FaultMetadata::runtime(code),
                success: BlockTarget::new(success, []),
                fault,
            },
            origin,
        )?;
        Ok(Flow {
            block: success,
            env: flow.env,
        })
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

    #[expect(
        clippy::too_many_lines,
        reason = "scalar operations and structural equality share one checked binary dispatch boundary"
    )]
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
        if matches!(operator, BinaryOp::Equal | BinaryOp::NotEqual)
            && !matches!(
                operand_type,
                Type::Unit | Type::Bool | Type::Int | Type::Float | Type::Text
            )
        {
            return self.lower_structural_equality(
                flow,
                operator,
                left,
                right,
                self.type_id(&operand_type)?,
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

    fn lower_structural_equality(
        &mut self,
        flow: Flow,
        operator: BinaryOp,
        left: ValueId,
        right: ValueId,
        operand_type: ValueTypeId,
        origin: Origin,
    ) -> Result<EvalFlow, LoweringError> {
        let equal = self.create_block()?;
        let not_equal = self.create_block()?;
        let join = self.create_block()?;
        let bool_type = self.type_id(&Type::Bool)?;
        let result = self
            .builder
            .append_block_parameter(join, bool_type)
            .map_err(LoweringError::from)?;
        self.branch_on_structural_equality(
            flow,
            left,
            right,
            operand_type,
            equal,
            not_equal,
            origin,
        )?;

        let true_value = match self.constant(
            Flow {
                block: equal,
                env: flow.env,
            },
            Constant::Bool(true),
            &Type::Bool,
            origin,
        )? {
            EvalFlow::Continue { value, .. } => value,
            EvalFlow::Terminated => {
                return Err(LoweringError::defect(
                    LoweringDefectCode::Builder,
                    "structural equality true constant unexpectedly terminated",
                ));
            }
        };
        self.terminate(
            equal,
            TerminatorKind::Jump(BlockTarget::new(join, [true_value])),
            origin,
        )?;
        let false_value = match self.constant(
            Flow {
                block: not_equal,
                env: flow.env,
            },
            Constant::Bool(false),
            &Type::Bool,
            origin,
        )? {
            EvalFlow::Continue { value, .. } => value,
            EvalFlow::Terminated => {
                return Err(LoweringError::defect(
                    LoweringDefectCode::Builder,
                    "structural equality false constant unexpectedly terminated",
                ));
            }
        };
        self.terminate(
            not_equal,
            TerminatorKind::Jump(BlockTarget::new(join, [false_value])),
            origin,
        )?;
        let joined = Flow {
            block: join,
            env: flow.env,
        };
        if operator == BinaryOp::NotEqual {
            self.one_instruction(
                joined,
                InstructionKind::BoolNot { value: result },
                bool_type,
                origin,
            )
        } else {
            Ok(EvalFlow::Continue {
                flow: joined,
                value: result,
            })
        }
    }

    #[expect(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "recursive representation dispatch keeps each exact typed equality shape auditable in one boundary"
    )]
    fn branch_on_structural_equality(
        &mut self,
        flow: Flow,
        left: ValueId,
        right: ValueId,
        ty: ValueTypeId,
        equal: BlockId,
        not_equal: BlockId,
        origin: Origin,
    ) -> Result<(), LoweringError> {
        for (side, value) in [("left", left), ("right", right)] {
            if self.builder.value_type(value) != Some(ty) {
                return Err(LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    format!("structural equality {side} operand has the wrong LCIR value type"),
                ));
            }
        }
        let value_type = self
            .builder
            .representations()
            .value_type(ty)
            .cloned()
            .ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    format!("structural equality references missing value type {ty}"),
                )
            })?;
        if let crate::ValueTypeKind::Transparent { base } = value_type.kind() {
            let left = match self.one_instruction(
                flow,
                InstructionKind::Unrefine { value: left },
                base,
                origin,
            )? {
                EvalFlow::Continue { value, .. } => value,
                EvalFlow::Terminated => {
                    return Err(LoweringError::defect(
                        LoweringDefectCode::Builder,
                        "left structural unrefine unexpectedly terminated",
                    ));
                }
            };
            let right = match self.one_instruction(
                flow,
                InstructionKind::Unrefine { value: right },
                base,
                origin,
            )? {
                EvalFlow::Continue { value, .. } => value,
                EvalFlow::Terminated => {
                    return Err(LoweringError::defect(
                        LoweringDefectCode::Builder,
                        "right structural unrefine unexpectedly terminated",
                    ));
                }
            };
            return self
                .branch_on_structural_equality(flow, left, right, base, equal, not_equal, origin);
        }

        let repr = self
            .builder
            .representations()
            .repr(value_type.repr())
            .copied()
            .ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    format!("structural equality references missing representation for {ty}"),
                )
            })?;
        match repr {
            crate::Repr::Zst => self.terminate(
                flow.block,
                TerminatorKind::Jump(BlockTarget::new(equal, [])),
                origin,
            ),
            crate::Repr::ManagedPointer if matches!(value_type.semantic(), Type::List(_)) => {
                let Type::List(element) = value_type.semantic() else {
                    unreachable!("managed List guard establishes its semantic shape")
                };
                let option = self.program.prelude.option.ok_or_else(|| {
                    LoweringError::defect(
                        LoweringDefectCode::InconsistentPlan,
                        "managed List equality requires the canonical Option type",
                    )
                })?;
                let option_type =
                    self.type_id(&Type::Nominal(option, vec![(**element).clone()]))?;
                self.branch_on_list_equality(
                    flow,
                    left,
                    right,
                    option_type,
                    equal,
                    not_equal,
                    origin,
                )
            }
            crate::Repr::Scalar(_) | crate::Repr::ImmortalText | crate::Repr::ManagedPointer => {
                let instruction = match value_type.semantic() {
                    Type::Bool => InstructionKind::BoolCompare {
                        predicate: BoolPredicate::Equal,
                        left,
                        right,
                    },
                    Type::Int => InstructionKind::IntCompare {
                        predicate: IntPredicate::Equal,
                        left,
                        right,
                    },
                    Type::Float => InstructionKind::FloatCompare {
                        predicate: FloatPredicate::OrderedEqual,
                        left,
                        right,
                    },
                    Type::Text => InstructionKind::TextCompare {
                        predicate: BoolPredicate::Equal,
                        left,
                        right,
                    },
                    _ => return Err(self.unsupported_reached("managed structural equality")),
                };
                let condition = match self.one_instruction(
                    flow,
                    instruction,
                    self.type_id(&Type::Bool)?,
                    origin,
                )? {
                    EvalFlow::Continue { value, .. } => value,
                    EvalFlow::Terminated => {
                        return Err(LoweringError::defect(
                            LoweringDefectCode::Builder,
                            "structural leaf comparison unexpectedly terminated",
                        ));
                    }
                };
                self.terminate(
                    flow.block,
                    TerminatorKind::Branch {
                        condition,
                        then_target: BlockTarget::new(equal, []),
                        else_target: BlockTarget::new(not_equal, []),
                    },
                    origin,
                )
            }
            crate::Repr::Product(product) => {
                let fields = self
                    .builder
                    .representations()
                    .product(product)
                    .map(|product| product.fields().to_vec())
                    .ok_or_else(|| {
                        LoweringError::defect(
                            LoweringDefectCode::InconsistentPlan,
                            format!("structural equality references missing product {product}"),
                        )
                    })?;
                let mut pairs = Vec::with_capacity(fields.len());
                let mut flow = flow;
                for (index, field_type) in fields.into_iter().enumerate() {
                    let field = u32::try_from(index).map_err(|_| LoweringError::ResourceLimit {
                        code: ResourceLimitCode::ProgramTooLarge,
                        message: "structural equality product field index exceeds u32".into(),
                    })?;
                    let left_field = match self.one_instruction(
                        flow,
                        InstructionKind::ProductExtract {
                            aggregate: left,
                            field,
                        },
                        field_type,
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
                                "left structural product extraction unexpectedly terminated",
                            ));
                        }
                    };
                    let right_field = match self.one_instruction(
                        flow,
                        InstructionKind::ProductExtract {
                            aggregate: right,
                            field,
                        },
                        field_type,
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
                                "right structural product extraction unexpectedly terminated",
                            ));
                        }
                    };
                    pairs.push((left_field, right_field, field_type));
                }
                self.branch_on_equal_fields(flow, &pairs, equal, not_equal, origin)
            }
            crate::Repr::Sum(sum) => {
                let variants = self
                    .builder
                    .representations()
                    .sum(sum)
                    .map(|sum| {
                        sum.variants()
                            .iter()
                            .map(|variant| variant.fields().to_vec())
                            .collect::<Vec<_>>()
                    })
                    .ok_or_else(|| {
                        LoweringError::defect(
                            LoweringDefectCode::InconsistentPlan,
                            format!("structural equality references missing sum {sum}"),
                        )
                    })?;
                self.branch_on_sum_equality(flow, left, right, &variants, equal, not_equal, origin)
            }
            crate::Repr::Uninhabited => Err(LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                "an uninhabited value reached structural equality lowering",
            )),
            crate::Repr::TaskHandle => {
                Err(self.unsupported_reached("Task handles do not have structural equality"))
            }
        }
    }

    fn branch_on_equal_fields(
        &mut self,
        mut flow: Flow,
        fields: &[(ValueId, ValueId, ValueTypeId)],
        equal: BlockId,
        not_equal: BlockId,
        origin: Origin,
    ) -> Result<(), LoweringError> {
        if fields.is_empty() {
            return self.terminate(
                flow.block,
                TerminatorKind::Jump(BlockTarget::new(equal, [])),
                origin,
            );
        }
        for (index, (left, right, ty)) in fields.iter().copied().enumerate() {
            let next = if index + 1 == fields.len() {
                equal
            } else {
                self.create_block()?
            };
            self.branch_on_structural_equality(flow, left, right, ty, next, not_equal, origin)?;
            flow.block = next;
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "the bounded List loop keeps length, proof, checked reads, and equality backedge visibly connected"
    )]
    fn branch_on_list_equality(
        &mut self,
        flow: Flow,
        left: ValueId,
        right: ValueId,
        option_type: ValueTypeId,
        equal: BlockId,
        not_equal: BlockId,
        origin: Origin,
    ) -> Result<(), LoweringError> {
        let integer = self.type_id(&Type::Int)?;
        let boolean = self.type_id(&Type::Bool)?;
        let left_length = match self.one_instruction(
            flow,
            InstructionKind::ListLength { list: left },
            integer,
            origin,
        )? {
            EvalFlow::Continue { value, .. } => value,
            EvalFlow::Terminated => {
                return Err(LoweringError::defect(
                    LoweringDefectCode::Builder,
                    "left List length unexpectedly terminated",
                ));
            }
        };
        let right_length = match self.one_instruction(
            flow,
            InstructionKind::ListLength { list: right },
            integer,
            origin,
        )? {
            EvalFlow::Continue { value, .. } => value,
            EvalFlow::Terminated => {
                return Err(LoweringError::defect(
                    LoweringDefectCode::Builder,
                    "right List length unexpectedly terminated",
                ));
            }
        };
        let same_length = match self.one_instruction(
            flow,
            InstructionKind::IntCompare {
                predicate: IntPredicate::Equal,
                left: left_length,
                right: right_length,
            },
            boolean,
            origin,
        )? {
            EvalFlow::Continue { value, .. } => value,
            EvalFlow::Terminated => {
                return Err(LoweringError::defect(
                    LoweringDefectCode::Builder,
                    "List length comparison unexpectedly terminated",
                ));
            }
        };
        let initialize = self.create_block()?;
        self.terminate(
            flow.block,
            TerminatorKind::Branch {
                condition: same_length,
                then_target: BlockTarget::new(initialize, []),
                else_target: BlockTarget::new(not_equal, []),
            },
            origin,
        )?;

        let zero = match self.constant(
            Flow {
                block: initialize,
                env: flow.env,
            },
            Constant::Int(0),
            &Type::Int,
            origin,
        )? {
            EvalFlow::Continue { value, .. } => value,
            EvalFlow::Terminated => {
                return Err(LoweringError::defect(
                    LoweringDefectCode::Builder,
                    "List equality zero index unexpectedly terminated",
                ));
            }
        };
        let header = self.create_block()?;
        let index = self
            .builder
            .append_block_parameter(header, integer)
            .map_err(LoweringError::from)?;
        self.terminate(
            initialize,
            TerminatorKind::Jump(BlockTarget::new(header, [zero])),
            origin,
        )?;
        let header_flow = Flow {
            block: header,
            env: flow.env,
        };
        let in_bounds = match self.one_instruction(
            header_flow,
            InstructionKind::IntCompare {
                predicate: IntPredicate::Less,
                left: index,
                right: left_length,
            },
            boolean,
            origin,
        )? {
            EvalFlow::Continue { value, .. } => value,
            EvalFlow::Terminated => {
                return Err(LoweringError::defect(
                    LoweringDefectCode::Builder,
                    "List equality bounds comparison unexpectedly terminated",
                ));
            }
        };
        let compare = self.create_block()?;
        self.terminate(
            header,
            TerminatorKind::Branch {
                condition: in_bounds,
                then_target: BlockTarget::new(compare, []),
                else_target: BlockTarget::new(equal, []),
            },
            origin,
        )?;
        let compare_flow = Flow {
            block: compare,
            env: flow.env,
        };
        let left_element = match self.one_instruction(
            compare_flow,
            InstructionKind::ListGet { list: left, index },
            option_type,
            origin,
        )? {
            EvalFlow::Continue { value, .. } => value,
            EvalFlow::Terminated => {
                return Err(LoweringError::defect(
                    LoweringDefectCode::Builder,
                    "left List equality read unexpectedly terminated",
                ));
            }
        };
        let right_element = match self.one_instruction(
            compare_flow,
            InstructionKind::ListGet { list: right, index },
            option_type,
            origin,
        )? {
            EvalFlow::Continue { value, .. } => value,
            EvalFlow::Terminated => {
                return Err(LoweringError::defect(
                    LoweringDefectCode::Builder,
                    "right List equality read unexpectedly terminated",
                ));
            }
        };
        let advance = self.create_block()?;
        self.branch_on_structural_equality(
            compare_flow,
            left_element,
            right_element,
            option_type,
            advance,
            not_equal,
            origin,
        )?;
        let next = match self.one_instruction(
            Flow {
                block: advance,
                env: flow.env,
            },
            InstructionKind::IntSuccessorBelow {
                value: index,
                upper_bound: left_length,
                proof: in_bounds,
            },
            integer,
            origin,
        )? {
            EvalFlow::Continue { value, .. } => value,
            EvalFlow::Terminated => {
                return Err(LoweringError::defect(
                    LoweringDefectCode::Builder,
                    "List equality successor unexpectedly terminated",
                ));
            }
        };
        self.terminate(
            advance,
            TerminatorKind::Jump(BlockTarget::new(header, [next])),
            origin,
        )
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn branch_on_sum_equality(
        &mut self,
        flow: Flow,
        left: ValueId,
        right: ValueId,
        variants: &[Vec<ValueTypeId>],
        equal: BlockId,
        not_equal: BlockId,
        origin: Origin,
    ) -> Result<(), LoweringError> {
        let mut left_cases = Vec::with_capacity(variants.len());
        let mut left_payloads = Vec::with_capacity(variants.len());
        for (variant, fields) in variants.iter().enumerate() {
            let block = self.create_block()?;
            let mut payload = Vec::with_capacity(fields.len());
            for field in fields {
                payload.push(
                    self.builder
                        .append_block_parameter(block, *field)
                        .map_err(LoweringError::from)?,
                );
            }
            left_cases.push(SumCase::new(
                u32::try_from(variant).map_err(|_| LoweringError::ResourceLimit {
                    code: ResourceLimitCode::ProgramTooLarge,
                    message: "structural equality sum variant index exceeds u32".into(),
                })?,
                block,
                [],
            ));
            left_payloads.push((block, payload));
        }
        self.terminate(
            flow.block,
            TerminatorKind::SumSwitch {
                scrutinee: left,
                cases: left_cases.into_boxed_slice(),
            },
            origin,
        )?;

        for (left_variant, (left_block, left_payload)) in left_payloads.into_iter().enumerate() {
            let mut right_cases = Vec::with_capacity(variants.len());
            let mut right_payloads = Vec::with_capacity(variants.len());
            for (right_variant, fields) in variants.iter().enumerate() {
                let block = self.create_block()?;
                let mut payload = Vec::with_capacity(fields.len());
                for field in fields {
                    payload.push(
                        self.builder
                            .append_block_parameter(block, *field)
                            .map_err(LoweringError::from)?,
                    );
                }
                right_cases.push(SumCase::new(
                    u32::try_from(right_variant).map_err(|_| LoweringError::ResourceLimit {
                        code: ResourceLimitCode::ProgramTooLarge,
                        message: "structural equality sum variant index exceeds u32".into(),
                    })?,
                    block,
                    [],
                ));
                right_payloads.push((block, payload));
            }
            self.terminate(
                left_block,
                TerminatorKind::SumSwitch {
                    scrutinee: right,
                    cases: right_cases.into_boxed_slice(),
                },
                origin,
            )?;
            for (right_variant, (right_block, right_payload)) in
                right_payloads.into_iter().enumerate()
            {
                if left_variant != right_variant {
                    self.terminate(
                        right_block,
                        TerminatorKind::Jump(BlockTarget::new(not_equal, [])),
                        origin,
                    )?;
                    continue;
                }
                let fields = variants.get(left_variant).ok_or_else(|| {
                    LoweringError::defect(
                        LoweringDefectCode::InconsistentPlan,
                        "structural equality lost a sum variant plan",
                    )
                })?;
                if left_payload.len() != fields.len() || right_payload.len() != fields.len() {
                    return Err(LoweringError::defect(
                        LoweringDefectCode::InconsistentPlan,
                        "structural equality sum payload shape changed during lowering",
                    ));
                }
                let pairs = left_payload
                    .iter()
                    .copied()
                    .zip(right_payload.iter().copied())
                    .zip(fields.iter().copied())
                    .map(|((left, right), ty)| (left, right, ty))
                    .collect::<Vec<_>>();
                self.branch_on_equal_fields(
                    Flow {
                        block: right_block,
                        env: flow.env,
                    },
                    &pairs,
                    equal,
                    not_equal,
                    origin,
                )?;
            }
        }
        Ok(())
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
                let incoming_unique = alternatives.iter().all(|environment| {
                    self.environments
                        .get(*environment, local)
                        .is_some_and(|value| self.unique_list_values.contains(&value))
                });
                let parameter = self
                    .builder
                    .append_block_parameter(join, self.local_type(local)?)
                    .map_err(LoweringError::from)?;
                if incoming_unique {
                    self.unique_list_values.insert(parameter);
                }
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
            if self.unique_list_values.contains(&incoming)
                && canonical_unique_list_loop_body(body, *outer_local)
            {
                self.unique_list_values.insert(parameter);
            }
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
            return match builtin {
                mir::Builtin::ListAdd | mir::Builtin::ListLength | mir::Builtin::ListGet => {
                    self.lower_list_builtin(flow, *builtin, arguments, expression)
                }
                mir::Builtin::TextMapNew
                | mir::Builtin::TextMapInsert
                | mir::Builtin::TextMapLength
                | mir::Builtin::TextMapGet => {
                    self.lower_text_map_builtin(flow, *builtin, arguments, expression)
                }
                _ => self.lower_builtin(flow, *builtin, arguments, expression),
            };
        }
        if let CallTarget::Dynamic { requirement } = target
            && let Some(receiver) = arguments.first()
        {
            let receiver_ty = self.dynamic_receiver_type(receiver, expression)?;
            if self.dyn_concepts.finite(&receiver_ty).is_some() {
                return self.lower_finite_dynamic_call(
                    flow,
                    *requirement,
                    arguments,
                    expression,
                    &receiver_ty,
                );
            }
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
            CallTarget::Dynamic { requirement } => {
                let receiver = arguments
                    .first()
                    .ok_or_else(|| self.unsupported_reached("dynamic call without receiver"))?;
                let receiver_ty = self.dynamic_receiver_type(receiver, expression)?;
                let choice = self
                    .dyn_concepts
                    .choice(&receiver_ty)
                    .ok_or_else(|| self.unsupported_reached("non-unique dynamic witness set"))?;
                substitution.static_call_key(
                    *requirement,
                    &mir::WitnessRef::Concrete(choice.witness()),
                    choice.concrete(),
                    &[],
                    &[],
                )
            }
            CallTarget::Builtin(_) => unreachable!("builtins return before direct-call planning"),
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
            .filter_map(|(index, parameter)| {
                InstanceSubstitution::new(self.program, &key)
                    .instantiate_type(&parameter.ty)
                    .ok()
                    .is_some_and(|ty| parameter.mutable || is_mutable_view(&ty))
                    .then_some(index)
            })
            .collect::<Vec<_>>();
        let mut inout_arguments = Vec::with_capacity(expected_inout.len());
        let origin = self.expression_origin(expression);
        for (index, argument) in arguments.iter().enumerate() {
            match argument {
                CallArgument::Value(argument) if expected_inout.contains(&index) => {
                    let (next_flow, value, place) =
                        self.lower_view_inout_argument(flow, argument, expression)?;
                    flow = next_flow;
                    lowered_arguments.push(value);
                    inout_arguments.push(InOutArgumentPlan {
                        parameter: index,
                        place,
                    });
                }
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
                    let plan = self.place_plan(place, PlaceUse::InOut)?;
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
        if callee_source.is_async {
            if !inout_arguments.is_empty() || !callee_source.call_plan.requires.is_empty() {
                return Err(self.unsupported_reached(
                    "async Task creation with inout arguments or preconditions",
                ));
            }
            let instance = self.instances.get(&key).ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    format!("async call target #{} has no LCIR instance", callee.0),
                )
            })?;
            return self.one_instruction(
                flow,
                InstructionKind::TaskCreate {
                    coroutine: instance,
                    arguments: lowered_arguments.into_boxed_slice(),
                },
                self.type_id(&expression.ty)?,
                origin,
            );
        }
        if !callee_source.call_plan.requires.is_empty() {
            let parameters = callee_source
                .params
                .iter()
                .zip(lowered_arguments.iter().copied())
                .map(|(parameter, value)| {
                    InstanceSubstitution::new(self.program, &key)
                        .instantiate_type(&parameter.ty)
                        .map(|ty| ContractOperand { value, ty })
                        .map_err(|error| {
                            instantiation_defect(self.source.id, Some(expression.id), error)
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let (receiver, arguments) = if callee_source.receiver.is_some() {
                let (receiver, arguments) = parameters.split_first().ok_or_else(|| {
                    LoweringError::defect(
                        LoweringDefectCode::InconsistentPlan,
                        "contracted receiver call has no receiver argument",
                    )
                })?;
                (Some(receiver.clone()), arguments.to_vec())
            } else {
                (None, parameters)
            };
            let context = ContractContext {
                receiver,
                record_candidate: None,
                result: None,
                arguments,
                old_receiver: None,
                old_arguments: Vec::new(),
                bindings: Vec::new(),
            };
            for contract in &callee_source.call_plan.requires {
                flow = self.lower_contract_check(
                    flow,
                    contract,
                    ContractFaultKind::Precondition,
                    expression.span,
                    &context,
                )?;
            }
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

    #[expect(
        clippy::too_many_lines,
        reason = "finite readonly dispatch keeps exact candidate calls and normal/fault joins in one auditable CFG construction"
    )]
    fn lower_finite_dynamic_call(
        &mut self,
        mut flow: Flow,
        requirement: mir::RequirementId,
        arguments: &[CallArgument],
        expression: &mir::Expr,
        receiver_ty: &Type,
    ) -> Result<EvalFlow, LoweringError> {
        if self
            .program
            .requirement(requirement)
            .is_some_and(|requirement| requirement.receiver == Some(mir::Receiver::Mutable))
        {
            return self.lower_finite_dynamic_mut_call(
                flow,
                requirement,
                arguments,
                expression,
                receiver_ty,
            );
        }
        let candidates = self
            .dyn_concepts
            .finite(receiver_ty)
            .ok_or_else(|| self.unsupported_reached("open dynamic witness set"))?
            .candidates()
            .to_vec();
        let mut values = Vec::with_capacity(arguments.len());
        for argument in arguments {
            let CallArgument::Value(argument) = argument else {
                return Err(self.unsupported_reached("finite dynamic inout dispatch"));
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
        let receiver = values
            .first()
            .copied()
            .ok_or_else(|| self.unsupported_reached("dynamic call without receiver"))?;
        let result_type = self.type_id(&expression.ty)?;
        let join = self.create_block()?;
        let result = self
            .builder
            .append_block_parameter(join, result_type)
            .map_err(LoweringError::from)?;
        let origin = self.expression_origin(expression);
        let mut cases = Vec::with_capacity(candidates.len());
        let mut plans = Vec::with_capacity(candidates.len());
        let substitution = InstanceSubstitution::new(self.program, self.key);
        for (variant, candidate) in candidates.iter().enumerate() {
            let variant = u32::try_from(variant).map_err(|_| LoweringError::ResourceLimit {
                code: ResourceLimitCode::ProgramTooLarge,
                message: "dynamic candidate set exceeds u32".to_owned(),
            })?;
            let key = substitution
                .static_call_key(
                    requirement,
                    &mir::WitnessRef::Concrete(candidate.witness()),
                    candidate.concrete(),
                    &[],
                    &[],
                )
                .map_err(|error| {
                    instantiation_defect(self.source.id, Some(expression.id), error)
                })?;
            let callee_source = self.program.function(key.source()).ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    "finite dynamic method disappeared",
                )
            })?;
            if callee_source.receiver == Some(mir::Receiver::Mutable)
                || !callee_source.call_plan.requires.is_empty()
            {
                return Err(
                    self.unsupported_reached("mutable or preconditioned finite dynamic dispatch")
                );
            }
            let instance = self.instances.get(&key).ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    format!(
                        "finite dynamic method {} has no LCIR instance",
                        key.source().0
                    ),
                )
            })?;
            let block = self.create_block()?;
            let payload = self
                .builder
                .append_block_parameter(block, self.type_id(candidate.concrete())?)
                .map_err(LoweringError::from)?;
            cases.push(crate::SumCase::new(variant, block, []));
            plans.push((block, payload, instance));
        }
        self.terminate(
            flow.block,
            TerminatorKind::DynSwitch {
                scrutinee: receiver,
                cases: cases.into_boxed_slice(),
            },
            origin,
        )?;
        for (block, payload, instance) in plans {
            let mut branch_arguments = Vec::with_capacity(values.len());
            branch_arguments.push(payload);
            branch_arguments.extend(values.iter().copied().skip(1));
            let effect = effect_for(self.effects, instance)?;
            if effect.contains(Effects::MAY_FAULT) {
                let normal = self.create_block()?;
                let value = self
                    .builder
                    .append_block_parameter(normal, result_type)
                    .map_err(LoweringError::from)?;
                let unwind = self.fault_target(Flow {
                    block,
                    env: flow.env,
                })?;
                self.terminate(
                    block,
                    TerminatorKind::Invoke {
                        callee: instance,
                        arguments: branch_arguments.into_boxed_slice(),
                        normal: ResultTarget::new(normal, []),
                        unwind,
                    },
                    origin,
                )?;
                self.terminate(
                    normal,
                    TerminatorKind::Jump(BlockTarget::new(join, [value])),
                    origin,
                )?;
            } else {
                let value = self
                    .builder
                    .append_instruction(
                        block,
                        InstructionKind::DirectCall {
                            callee: instance,
                            arguments: branch_arguments.into_boxed_slice(),
                        },
                        &[result_type],
                        origin,
                    )
                    .map_err(LoweringError::from)?[0];
                self.terminate(
                    block,
                    TerminatorKind::Jump(BlockTarget::new(join, [value])),
                    origin,
                )?;
            }
        }
        Ok(EvalFlow::Continue {
            flow: Flow {
                block: join,
                env: flow.env,
            },
            value: result,
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "finite mutable dispatch keeps candidate-specific inout calls, normal/fault fresh boxing, and owner writeback in one auditable CFG construction"
    )]
    fn lower_finite_dynamic_mut_call(
        &mut self,
        flow: Flow,
        requirement: mir::RequirementId,
        arguments: &[CallArgument],
        expression: &mir::Expr,
        receiver_ty: &Type,
    ) -> Result<EvalFlow, LoweringError> {
        let candidates = self
            .dyn_concepts
            .finite(receiver_ty)
            .ok_or_else(|| self.unsupported_reached("open mutable dynamic witness set"))?
            .candidates()
            .to_vec();
        let receiver = arguments
            .first()
            .ok_or_else(|| self.unsupported_reached("mutable dynamic call without receiver"))?;
        let owner = match receiver {
            CallArgument::InOut(owner) => owner,
            CallArgument::Value(receiver) => {
                let ExprKind::ReborrowView {
                    owner,
                    mutable: true,
                    ..
                } = &receiver.kind
                else {
                    return Err(self.unsupported_reached(
                        "mutable finite dynamic receiver without first-class owner storage",
                    ));
                };
                owner
            }
        };
        let owner = self.place_plan(owner, PlaceUse::InOut)?;
        let owner_semantic = self
            .builder
            .representations()
            .value_type(owner.leaf_type())
            .ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    "mutable dynamic owner type disappeared",
                )
            })?
            .semantic();
        let owner_candidates = self
            .dyn_concepts
            .finite(owner_semantic)
            .ok_or_else(|| self.unsupported_reached("mutable dynamic owner is not finite"))?;
        if owner_candidates.candidates() != candidates.as_slice() {
            return Err(
                self.unsupported_reached("mutable dynamic reborrow changes its candidate catalog")
            );
        }
        let origin = self.expression_origin(expression);
        let EvalFlow::Continue {
            mut flow,
            value: receiver,
        } = self.read_place(flow, &owner, origin)?
        else {
            return Err(LoweringError::defect(
                LoweringDefectCode::Builder,
                "mutable dynamic owner read unexpectedly terminated",
            ));
        };
        let base_environment = flow.env;
        let mut values = vec![receiver];
        for argument in arguments.iter().skip(1) {
            let CallArgument::Value(argument) = argument else {
                return Err(self.unsupported_reached(
                    "finite dynamic method has an additional inout argument",
                ));
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

        let mut cases = Vec::with_capacity(candidates.len());
        let mut plans = Vec::with_capacity(candidates.len());
        let substitution = InstanceSubstitution::new(self.program, self.key);
        for (variant, candidate) in candidates.iter().enumerate() {
            let variant = u32::try_from(variant).map_err(|_| LoweringError::ResourceLimit {
                code: ResourceLimitCode::ProgramTooLarge,
                message: "dynamic candidate set exceeds u32".to_owned(),
            })?;
            let key = substitution
                .static_call_key(
                    requirement,
                    &mir::WitnessRef::Concrete(candidate.witness()),
                    candidate.concrete(),
                    &[],
                    &[],
                )
                .map_err(|error| {
                    instantiation_defect(self.source.id, Some(expression.id), error)
                })?;
            let callee_source = self.program.function(key.source()).ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    "finite mutable dynamic method disappeared",
                )
            })?;
            if !callee_source.call_plan.requires.is_empty() {
                return Err(
                    self.unsupported_reached("preconditioned finite mutable dynamic dispatch")
                );
            }
            let instance = self.instances.get(&key).ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    format!(
                        "finite mutable dynamic method {} has no LCIR instance",
                        key.source().0
                    ),
                )
            })?;
            let block = self.create_block()?;
            let payload = self
                .builder
                .append_block_parameter(block, self.type_id(candidate.concrete())?)
                .map_err(LoweringError::from)?;
            cases.push(crate::SumCase::new(variant, block, []));
            plans.push((variant, block, payload, instance));
        }
        self.terminate(
            flow.block,
            TerminatorKind::DynSwitch {
                scrutinee: receiver,
                cases: cases.into_boxed_slice(),
            },
            origin,
        )?;

        let result_type = self.type_id(&expression.ty)?;
        let mut continuations = Vec::with_capacity(plans.len());
        for (variant, block, payload, instance) in plans {
            let candidate_type = self.builder.value_type(payload).ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    "finite mutable payload type disappeared",
                )
            })?;
            let mut branch_arguments = Vec::with_capacity(values.len());
            branch_arguments.push(payload);
            branch_arguments.extend(values.iter().copied().skip(1));
            let effect = effect_for(self.effects, instance)?;
            if effect.contains(Effects::MAY_FAULT) {
                let normal = self.create_block()?;
                let result = self
                    .builder
                    .append_block_parameter(normal, result_type)
                    .map_err(LoweringError::from)?;
                let normal_writeback = self
                    .builder
                    .append_block_parameter(normal, candidate_type)
                    .map_err(LoweringError::from)?;
                let fault = self.create_block()?;
                let fault_writeback = self
                    .builder
                    .append_block_parameter(fault, candidate_type)
                    .map_err(LoweringError::from)?;
                self.terminate(
                    block,
                    TerminatorKind::Invoke {
                        callee: instance,
                        arguments: branch_arguments.into_boxed_slice(),
                        normal: ResultTarget::new(normal, []),
                        unwind: UnwindTarget::new(fault, []),
                    },
                    origin,
                )?;

                let fault_flow = Flow {
                    block: fault,
                    env: base_environment,
                };
                let EvalFlow::Continue {
                    flow: fault_flow,
                    value: fault_box,
                } = self.one_instruction(
                    fault_flow,
                    InstructionKind::DynConstruct {
                        variant,
                        value: fault_writeback,
                    },
                    owner.leaf_type(),
                    origin,
                )?
                else {
                    return Err(LoweringError::defect(
                        LoweringDefectCode::Builder,
                        "fault-edge dynamic boxing unexpectedly terminated",
                    ));
                };
                let fault_flow = self.write_place(fault_flow, &owner, fault_box, origin)?;
                let propagation = self.fault_target(fault_flow)?;
                self.terminate(
                    fault_flow.block,
                    TerminatorKind::Jump(BlockTarget::new(
                        propagation.block,
                        propagation.arguments,
                    )),
                    origin,
                )?;

                let normal_flow = Flow {
                    block: normal,
                    env: base_environment,
                };
                let EvalFlow::Continue {
                    flow: normal_flow,
                    value: normal_box,
                } = self.one_instruction(
                    normal_flow,
                    InstructionKind::DynConstruct {
                        variant,
                        value: normal_writeback,
                    },
                    owner.leaf_type(),
                    origin,
                )?
                else {
                    return Err(LoweringError::defect(
                        LoweringDefectCode::Builder,
                        "normal-edge dynamic boxing unexpectedly terminated",
                    ));
                };
                let normal_flow = self.write_place(normal_flow, &owner, normal_box, origin)?;
                continuations.push(EvalFlow::Continue {
                    flow: normal_flow,
                    value: result,
                });
            } else {
                let results = self
                    .builder
                    .append_instruction(
                        block,
                        InstructionKind::DirectCall {
                            callee: instance,
                            arguments: branch_arguments.into_boxed_slice(),
                        },
                        &[result_type, candidate_type],
                        origin,
                    )
                    .map_err(LoweringError::from)?;
                let [result, writeback] = results.as_ref() else {
                    return Err(LoweringError::defect(
                        LoweringDefectCode::Builder,
                        "finite mutable call did not return result and receiver writeback",
                    ));
                };
                let branch_flow = Flow {
                    block,
                    env: base_environment,
                };
                let EvalFlow::Continue {
                    flow: branch_flow,
                    value: boxed,
                } = self.one_instruction(
                    branch_flow,
                    InstructionKind::DynConstruct {
                        variant,
                        value: *writeback,
                    },
                    owner.leaf_type(),
                    origin,
                )?
                else {
                    return Err(LoweringError::defect(
                        LoweringDefectCode::Builder,
                        "dynamic writeback boxing unexpectedly terminated",
                    ));
                };
                let branch_flow = self.write_place(branch_flow, &owner, boxed, origin)?;
                continuations.push(EvalFlow::Continue {
                    flow: branch_flow,
                    value: *result,
                });
            }
        }
        self.merge_evaluations(continuations, base_environment, &expression.ty, origin)
    }

    fn dynamic_receiver_type(
        &self,
        argument: &CallArgument,
        expression: &mir::Expr,
    ) -> Result<Type, LoweringError> {
        let source = match argument {
            CallArgument::Value(value) => &value.ty,
            CallArgument::InOut(place) if place.projection.is_empty() => {
                self.local_types.get(&place.local).ok_or_else(|| {
                    LoweringError::defect(
                        LoweringDefectCode::InconsistentPlan,
                        format!("dynamic receiver local #{} disappeared", place.local.0),
                    )
                })?
            }
            CallArgument::InOut(_) => {
                return Err(self.unsupported_reached("projected dynamic receiver"));
            }
        };
        InstanceSubstitution::new(self.program, self.key)
            .instantiate_type(source)
            .map_err(|error| instantiation_defect(self.source.id, Some(expression.id), error))
    }

    fn lower_view_inout_argument(
        &mut self,
        flow: Flow,
        argument: &mir::Expr,
        call: &mir::Expr,
    ) -> Result<(Flow, ValueId, PlacePlan), LoweringError> {
        let origin = self.expression_origin(call);
        match &argument.kind {
            ExprKind::MakeView {
                value,
                writeback: Some(writeback),
                witness,
                mutable: true,
                ..
            } => {
                let view_ty = InstanceSubstitution::new(self.program, self.key)
                    .instantiate_type(&argument.ty)
                    .map_err(|error| {
                        instantiation_defect(self.source.id, Some(argument.id), error)
                    })?;
                let choice = self.dyn_concepts.choice(&view_ty).ok_or_else(|| {
                    self.unsupported_reached("non-unique mutable dynamic witness set")
                })?;
                if witness != &mir::WitnessRef::Concrete(choice.witness()) {
                    return Err(self.unsupported_reached("mutable dynamic witness mismatch"));
                }
                let EvalFlow::Continue { flow, value } = self.lower_expr(flow, value)? else {
                    return Err(LoweringError::defect(
                        LoweringDefectCode::InconsistentPlan,
                        "mutable dynamic receiver terminated before call",
                    ));
                };
                let place = self.place_plan(writeback, PlaceUse::InOut)?;
                if self.builder.value_type(value) != Some(place.leaf_type()) {
                    return Err(LoweringError::defect(
                        LoweringDefectCode::InconsistentPlan,
                        "mutable dynamic receiver does not match its writeback place",
                    ));
                }
                Ok((flow, value, place))
            }
            ExprKind::ReborrowView {
                owner,
                mutable: true,
                ..
            } => {
                let place = self.place_plan(owner, PlaceUse::InOut)?;
                let EvalFlow::Continue { flow, value } = self.read_place(flow, &place, origin)?
                else {
                    return Err(LoweringError::defect(
                        LoweringDefectCode::Builder,
                        "mutable dynamic reborrow unexpectedly terminated",
                    ));
                };
                Ok((flow, value, place))
            }
            _ => Err(self.unsupported_reached(
                "mutable interface argument without an exact writeback place",
            )),
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one exhaustive typed builtin boundary keeps classification and lowering aligned"
    )]
    fn lower_builtin(
        &mut self,
        mut flow: Flow,
        builtin: mir::Builtin,
        arguments: &[CallArgument],
        expression: &mir::Expr,
    ) -> Result<EvalFlow, LoweringError> {
        let mut values = Vec::with_capacity(arguments.len());
        for argument in arguments {
            let CallArgument::Value(argument) = argument else {
                return Err(self.unsupported_reached("builtin inout argument"));
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
        let origin = self.expression_origin(expression);
        let kind = match (builtin, values.as_slice()) {
            (mir::Builtin::TextLength, [text]) => Some(InstructionKind::TextLength { text: *text }),
            (mir::Builtin::TextConcat, [left, right]) => InstructionKind::TextConcat {
                left: *left,
                right: *right,
            }
            .into(),
            (mir::Builtin::TextGet, [text, index]) => InstructionKind::TextGet {
                text: *text,
                index: *index,
                missing_variant: 0,
                found_variant: 1,
            }
            .into(),
            (mir::Builtin::TextContains, [text, needle]) => InstructionKind::TextContains {
                text: *text,
                needle: *needle,
            }
            .into(),
            (mir::Builtin::ParseInt, [text]) => InstructionKind::ParseInt {
                text: *text,
                ok_variant: 0,
                error_variant: 1,
                invalid_syntax_variant: 0,
                out_of_range_variant: 1,
            }
            .into(),
            (mir::Builtin::ParseFloat, [text]) => InstructionKind::ParseFloat {
                text: *text,
                ok_variant: 0,
                error_variant: 1,
                invalid_syntax_variant: 0,
                out_of_range_variant: 1,
            }
            .into(),
            (mir::Builtin::FormatFloat, [value]) => {
                InstructionKind::FormatFloat { value: *value }.into()
            }
            (mir::Builtin::IsFinite, [value]) => {
                return self.lower_float_is_finite(flow, *value, origin);
            }
            (mir::Builtin::DurationMilliseconds, [milliseconds]) => {
                let EvalFlow::Continue { flow, value: zero } =
                    self.constant(flow, Constant::Int(0), &Type::Int, origin)?
                else {
                    return Err(self.unsupported_reached("Duration zero constant"));
                };
                let EvalFlow::Continue {
                    flow,
                    value: nonnegative,
                } = self.one_instruction(
                    flow,
                    InstructionKind::IntCompare {
                        predicate: IntPredicate::GreaterEqual,
                        left: *milliseconds,
                        right: zero,
                    },
                    self.type_id(&Type::Bool)?,
                    origin,
                )?
                else {
                    return Err(self.unsupported_reached("Duration nonnegative comparison"));
                };
                let success = self.create_block()?;
                let fault = self.fault_target(flow)?;
                self.terminate(
                    flow.block,
                    TerminatorKind::Assert {
                        condition: nonnegative,
                        metadata: FaultMetadata::runtime(FaultCode::InvalidDuration),
                        success: BlockTarget::new(success, []),
                        fault,
                    },
                    origin,
                )?;
                return self.one_instruction(
                    Flow {
                        block: success,
                        env: flow.env,
                    },
                    InstructionKind::ProductConstruct {
                        fields: [*milliseconds].into(),
                    },
                    self.type_id(&expression.ty)?,
                    origin,
                );
            }
            (mir::Builtin::DurationAsMilliseconds, [duration]) => {
                return self.one_instruction(
                    flow,
                    InstructionKind::ProductExtract {
                        aggregate: *duration,
                        field: 0,
                    },
                    self.type_id(&expression.ty)?,
                    origin,
                );
            }
            _ => return Err(self.unsupported_reached("unsupported builtin")),
        };
        self.one_instruction(
            flow,
            kind.expect("supported non-control-flow builtin has an instruction"),
            self.type_id(&expression.ty)?,
            origin,
        )
    }

    fn lower_list_builtin(
        &mut self,
        mut flow: Flow,
        builtin: mir::Builtin,
        arguments: &[CallArgument],
        expression: &mir::Expr,
    ) -> Result<EvalFlow, LoweringError> {
        let origin = self.expression_origin(expression);
        if builtin == mir::Builtin::ListAdd {
            let [CallArgument::InOut(receiver), CallArgument::Value(value)] = arguments else {
                return Err(self.unsupported_reached("List.add argument shape"));
            };
            let receiver = self.place_plan(receiver, PlaceUse::InOut)?;
            let direct_local = receiver.steps().is_empty();
            let EvalFlow::Continue {
                flow: next_flow,
                value: list,
            } = self.read_place(flow, &receiver, origin)?
            else {
                return Err(LoweringError::defect(
                    LoweringDefectCode::Builder,
                    "typed List.add receiver read unexpectedly terminated",
                ));
            };
            let EvalFlow::Continue {
                flow: next_flow,
                value,
            } = self.lower_expr(next_flow, value)?
            else {
                return Ok(EvalFlow::Terminated);
            };
            let EvalFlow::Continue {
                flow: next_flow,
                value: appended,
            } = (if direct_local && self.unique_list_values.contains(&list) {
                self.one_trusted_instruction(
                    next_flow,
                    InstructionKind::ListAppendUnique { list, value },
                    receiver.leaf_type(),
                    origin,
                )?
            } else {
                self.one_instruction(
                    next_flow,
                    InstructionKind::ListAppend { list, value },
                    receiver.leaf_type(),
                    origin,
                )?
            })
            else {
                return Err(LoweringError::defect(
                    LoweringDefectCode::Builder,
                    "typed List.append unexpectedly terminated",
                ));
            };
            flow = self.write_place(next_flow, &receiver, appended, origin)?;
            return self.constant(flow, Constant::Unit, &Type::Unit, origin);
        }

        let values = arguments
            .iter()
            .map(|argument| match argument {
                CallArgument::Value(value) => Ok(value),
                CallArgument::InOut(_) => {
                    Err(self.unsupported_reached("List builtin inout argument"))
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut lowered = Vec::with_capacity(values.len());
        for value in values {
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
        let kind = match (builtin, lowered.as_slice()) {
            (mir::Builtin::ListLength, [list]) => InstructionKind::ListLength { list: *list },
            (mir::Builtin::ListGet, [list, index]) => InstructionKind::ListGet {
                list: *list,
                index: *index,
            },
            _ => return Err(self.unsupported_reached("unsupported List builtin")),
        };
        self.one_instruction(flow, kind, self.type_id(&expression.ty)?, origin)
    }

    fn lower_text_map_builtin(
        &mut self,
        mut flow: Flow,
        builtin: mir::Builtin,
        arguments: &[CallArgument],
        expression: &mir::Expr,
    ) -> Result<EvalFlow, LoweringError> {
        let mut lowered = Vec::with_capacity(arguments.len());
        for argument in arguments {
            let CallArgument::Value(value) = argument else {
                return Err(self.unsupported_reached("TextMap builtin inout argument"));
            };
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
        let kind = match (builtin, lowered.as_slice()) {
            (mir::Builtin::TextMapNew, []) => InstructionKind::TextMapConstruct,
            (mir::Builtin::TextMapInsert, [map, key, value]) => InstructionKind::TextMapInsert {
                map: *map,
                key: *key,
                value: *value,
            },
            (mir::Builtin::TextMapLength, [map]) => InstructionKind::TextMapLength { map: *map },
            (mir::Builtin::TextMapGet, [map, key]) => InstructionKind::TextMapGet {
                map: *map,
                key: *key,
            },
            _ => return Err(self.unsupported_reached("unsupported TextMap builtin")),
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
        let program = mir::Program::default();

        assert!(!scan_effect_block(&program, &block, &mut summary));
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
