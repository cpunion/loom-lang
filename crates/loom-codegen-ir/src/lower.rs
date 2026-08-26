use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::fmt;

use loom_core::Span;
use loom_mir::{
    self as mir, BinaryOp, CallArgument, CallTarget, ExprId, ExprKind, FunctionId, LocalId,
    StatementKind, Type, UnaryOp,
};

use crate::{
    ArtifactRootRequest, BlockId, BlockTarget, BoolPredicate, BuildError, BuildErrorCode,
    CheckedArtifact, CheckedIntBinaryOp, Constant, Effects, FloatBinaryOp, FloatPredicate,
    FunctionBuilder, InstanceId, InstructionKind, IntPredicate, Origin, ProgramBuilder,
    ResultTarget, Signature, SourceRoots, TargetLayout, Terminator, TerminatorKind, UnwindTarget,
    ValueId, ValueTypeId, analyze_source_reachability,
};

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
    TupleBinding,
    AssertionCleanup,
    DeferredCleanup,
    TupleValue,
    ListValue,
    PatternMatch,
    NominalValue,
    RefinedValue,
    StaticDispatch,
    DynamicDispatch,
    BuiltinCall,
    GenericCall,
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
            Self::TupleBinding => "TupleBinding",
            Self::AssertionCleanup => "AssertionCleanup",
            Self::DeferredCleanup => "DeferredCleanup",
            Self::TupleValue => "TupleValue",
            Self::ListValue => "ListValue",
            Self::PatternMatch => "PatternMatch",
            Self::NominalValue => "NominalValue",
            Self::RefinedValue => "RefinedValue",
            Self::StaticDispatch => "StaticDispatch",
            Self::DynamicDispatch => "DynamicDispatch",
            Self::BuiltinCall => "BuiltinCall",
            Self::GenericCall => "GenericCall",
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
pub fn lower_scalar_artifact(
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

    let mut classifier = Classifier::new();
    for function in &graph.functions {
        let source = mir.function(*function).ok_or_else(|| {
            LoweringError::defect(
                LoweringDefectCode::SourceGraph,
                format!("reachable function #{} does not exist", function.0),
            )
        })?;
        classifier.classify_function(source);
    }
    if !classifier.items.is_empty() {
        return Ok(LoweringOutcome::Unsupported(SupportReport {
            items: classifier.items,
        }));
    }

    let summaries = graph
        .functions
        .iter()
        .map(|id| {
            let function = mir.function(*id).ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::SourceGraph,
                    format!("reachable function #{} disappeared", id.0),
                )
            })?;
            Ok((*id, summarize_effects(function)))
        })
        .collect::<Result<BTreeMap<_, _>, LoweringError>>()?;
    let effects = solve_effects(&summaries)?;

    let mut builder = ProgramBuilder::new(target);
    let mut instances = BTreeMap::new();
    for function_id in &graph.functions {
        let function = mir.function(*function_id).ok_or_else(|| {
            LoweringError::defect(
                LoweringDefectCode::SourceGraph,
                format!("reachable function #{} disappeared", function_id.0),
            )
        })?;
        let params = function
            .params
            .iter()
            .map(|parameter| required_type(&builder, &parameter.ty))
            .collect::<Result<Vec<_>, _>>()?;
        let result = required_type(&builder, &function.return_ty)?;
        let effect = effect_for(&effects, *function_id)?;
        let instance = builder
            .declare_function(
                Origin {
                    source_function: function.id,
                    expression: None,
                    span: function.span,
                },
                &function.name,
                Signature::new(params, effect_result(result)),
                effect,
            )
            .map_err(LoweringError::from)?;
        instances.insert(*function_id, instance);
    }

    for function_id in &graph.functions {
        let source = mir.function(*function_id).ok_or_else(|| {
            LoweringError::defect(
                LoweringDefectCode::SourceGraph,
                format!("reachable function #{} disappeared", function_id.0),
            )
        })?;
        let instance = instances.get(function_id).copied().ok_or_else(|| {
            LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                format!("function #{} has no LCIR declaration", function_id.0),
            )
        })?;
        let function_builder = builder.function(instance).map_err(LoweringError::from)?;
        FunctionLowerer::new(source, function_builder, &instances, &effects).lower()?;
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
            instances.get(source).copied().ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    format!("root function #{} has no LCIR instance", source.0),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let roots = if selected.tests {
        ArtifactRootRequest::tests(lowered_roots)
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

// Keeps signature construction visibly separate from semantic type lookup;
// later representation expansion may attach ABI-specific result planning.
const fn effect_result(result: ValueTypeId) -> ValueTypeId {
    result
}

fn required_type(builder: &ProgramBuilder, ty: &Type) -> Result<ValueTypeId, LoweringError> {
    builder.type_id(ty).ok_or_else(|| {
        LoweringError::defect(
            LoweringDefectCode::InconsistentPlan,
            format!("classified scalar type {ty:?} has no LCIR representation"),
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
    Ok(SelectedRoots {
        source,
        ordered,
        tests,
    })
}

fn is_valid_test_return(program: &mir::Program, ty: &Type) -> bool {
    if *ty == Type::Unit {
        return true;
    }
    let Some(result) = program.prelude.result else {
        return false;
    };
    matches!(
        ty,
        Type::Nominal(type_id, arguments)
            if *type_id == result
                && arguments.len() == 2
                && arguments.first() == Some(&Type::Unit)
    )
}

const fn is_scalar_type(ty: &Type) -> bool {
    matches!(ty, Type::Unit | Type::Bool | Type::Int | Type::Float)
}

const fn is_supported_expression_type(ty: &Type) -> bool {
    is_scalar_type(ty) || matches!(ty, Type::Never)
}

struct Classifier {
    items: Vec<UnsupportedItem>,
}

impl Classifier {
    const fn new() -> Self {
        Self { items: Vec::new() }
    }

    fn classify_function(&mut self, function: &mir::Function) {
        let base = format!("function[{}]", function.id.0);
        if function.type_parameters != 0 {
            self.function_item(UnsupportedFeature::GenericFunction, function, &base);
        }
        if function.is_async || !function.suspension_points.is_empty() {
            self.function_item(UnsupportedFeature::AsyncFunction, function, &base);
        }
        if !function.witness_params.is_empty() || function.witness_prefix_count != 0 {
            self.function_item(UnsupportedFeature::WitnessParameters, function, &base);
        }
        if function.receiver == Some(mir::Receiver::Mutable) {
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
            if parameter.mutable {
                self.item(
                    UnsupportedFeature::MutableParameter,
                    function.id,
                    None,
                    parameter.span,
                    path.clone(),
                );
            }
            if !is_scalar_type(&parameter.ty) {
                self.item(
                    UnsupportedFeature::SignatureType,
                    function.id,
                    None,
                    parameter.span,
                    path,
                );
            }
        }
        if !is_scalar_type(&function.return_ty) {
            self.item(
                UnsupportedFeature::SignatureType,
                function.id,
                None,
                function.span,
                format!("{base}.return_ty"),
            );
        }
        // Function-local declarations include values from syntactically dead
        // regions. Reachable expressions below carry their checked types, so
        // classifying uses rather than the whole declaration table keeps DCE
        // exact while still rejecting every executable non-scalar value.
        self.visit_block(function.id, &function.body, &format!("{base}.body"));
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
        function: FunctionId,
        expression: &mir::Expr,
        path: &str,
    ) {
        self.item(
            feature,
            function,
            Some(expression.id),
            expression.span,
            path.to_owned(),
        );
    }

    fn projected_place(
        &mut self,
        function: FunctionId,
        expression: Option<&mir::Expr>,
        place: &mir::Place,
        span: Span,
        path: &str,
    ) {
        if !place.projection.is_empty() {
            self.item(
                UnsupportedFeature::ProjectedPlace,
                function,
                expression.map(|value| value.id),
                span,
                path.to_owned(),
            );
        }
    }

    fn visit_block(&mut self, function: FunctionId, block: &mir::Block, path: &str) -> bool {
        for (index, statement) in block.statements.iter().enumerate() {
            let statement_path = format!("{path}.statements[{index}]");
            if !self.visit_statement(function, statement, &statement_path) {
                return false;
            }
        }
        if let Some(tail) = block.tail.as_deref() {
            self.visit_expr(function, tail, &format!("{path}.tail"))
        } else {
            true
        }
    }

    #[allow(clippy::too_many_lines)]
    fn visit_statement(
        &mut self,
        function: FunctionId,
        statement: &mir::Statement,
        path: &str,
    ) -> bool {
        match &statement.kind {
            StatementKind::Let { value, .. } => {
                self.visit_expr(function, value, &format!("{path}.value"))
            }
            StatementKind::LetTuple { value, .. } => {
                if !self.visit_expr(function, value, &format!("{path}.value")) {
                    return false;
                }
                self.item(
                    UnsupportedFeature::TupleBinding,
                    function,
                    Some(value.id),
                    statement.span,
                    path.to_owned(),
                );
                true
            }
            StatementKind::ForRange {
                start, end, body, ..
            } => {
                let start_continues = self.visit_expr(function, start, &format!("{path}.start"));
                if !start_continues {
                    return false;
                }
                let end_continues = self.visit_expr(function, end, &format!("{path}.end"));
                if !end_continues {
                    return false;
                }
                self.visit_block(function, body, &format!("{path}.body"));
                true
            }
            StatementKind::Assign { place, value } => {
                if !self.visit_expr(function, value, &format!("{path}.value")) {
                    return false;
                }
                self.projected_place(
                    function,
                    None,
                    place,
                    statement.span,
                    &format!("{path}.place"),
                );
                true
            }
            StatementKind::Assert { condition } => {
                if !self.visit_expr(function, condition, &format!("{path}.condition")) {
                    return false;
                }
                self.item(
                    UnsupportedFeature::AssertionCleanup,
                    function,
                    Some(condition.id),
                    statement.span,
                    path.to_owned(),
                );
                true
            }
            StatementKind::Evaluate(expression) => {
                self.visit_expr(function, expression, &format!("{path}.value"))
            }
            StatementKind::Defer(cleanup) => {
                self.item(
                    UnsupportedFeature::DeferredCleanup,
                    function,
                    None,
                    statement.span,
                    path.to_owned(),
                );
                self.visit_block(function, cleanup, &format!("{path}.cleanup"));
                true
            }
            StatementKind::Return(value) => {
                if let Some(value) = value {
                    self.visit_expr(function, value, &format!("{path}.value"));
                }
                false
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn visit_expr(&mut self, function: FunctionId, expression: &mir::Expr, path: &str) -> bool {
        let continues = match &expression.kind {
            ExprKind::Constant(mir::Constant::Text(_)) => {
                self.expression_item(UnsupportedFeature::TextConstant, function, expression, path);
                true
            }
            ExprKind::Constant(_) => true,
            ExprKind::Tuple(elements) => {
                if !self.visit_exprs(function, elements, &format!("{path}.elements")) {
                    return false;
                }
                self.expression_item(UnsupportedFeature::TupleValue, function, expression, path);
                expression.ty != Type::Never
            }
            ExprKind::List(elements) => {
                if !self.visit_exprs(function, elements, &format!("{path}.elements")) {
                    return false;
                }
                self.expression_item(UnsupportedFeature::ListValue, function, expression, path);
                expression.ty != Type::Never
            }
            ExprKind::Copy(place) | ExprKind::Move(place) => {
                self.projected_place(
                    function,
                    Some(expression),
                    place,
                    expression.span,
                    &format!("{path}.place"),
                );
                true
            }
            ExprKind::Unary(_, operand) => {
                self.visit_expr(function, operand, &format!("{path}.operand"))
                    && expression.ty != Type::Never
            }
            ExprKind::Binary(operator, left, right) => {
                if self.visit_expr(function, left, &format!("{path}.left")) {
                    let right_continues =
                        self.visit_expr(function, right, &format!("{path}.right"));
                    right_continues || matches!(operator, BinaryOp::And | BinaryOp::Or)
                } else {
                    false
                }
            }
            ExprKind::Block(block) => {
                self.visit_block(function, block, &format!("{path}.block"))
                    && expression.ty != Type::Never
            }
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                if !self.visit_expr(function, condition, &format!("{path}.condition")) {
                    return false;
                }
                let then_continues =
                    self.visit_block(function, then_branch, &format!("{path}.then"));
                let else_continues =
                    self.visit_block(function, else_branch, &format!("{path}.else"));
                then_continues || else_continues
            }
            ExprKind::Match { scrutinee, arms } => {
                if !self.visit_expr(function, scrutinee, &format!("{path}.scrutinee")) {
                    return false;
                }
                self.expression_item(UnsupportedFeature::PatternMatch, function, expression, path);
                let mut continues = false;
                for (index, arm) in arms.iter().enumerate() {
                    continues |= self.visit_expr(
                        function,
                        &arm.value,
                        &format!("{path}.arms[{index}].value"),
                    );
                }
                continues
            }
            ExprKind::Record { fields, .. } => {
                if !self.visit_exprs(function, fields, &format!("{path}.fields")) {
                    return false;
                }
                self.expression_item(UnsupportedFeature::NominalValue, function, expression, path);
                expression.ty != Type::Never
            }
            ExprKind::Variant { payload, .. } => {
                if !self.visit_exprs(function, payload, &format!("{path}.payload")) {
                    return false;
                }
                self.expression_item(UnsupportedFeature::NominalValue, function, expression, path);
                expression.ty != Type::Never
            }
            ExprKind::Refine { value, .. } | ExprKind::Unrefine(value) => {
                if !self.visit_expr(function, value, &format!("{path}.value")) {
                    return false;
                }
                self.expression_item(UnsupportedFeature::RefinedValue, function, expression, path);
                expression.ty != Type::Never
            }
            ExprKind::Call {
                target,
                type_arguments,
                arguments,
                witnesses,
            } => {
                for (index, argument) in arguments.iter().enumerate() {
                    match argument {
                        CallArgument::Value(value) => {
                            if !self.visit_expr(
                                function,
                                value,
                                &format!("{path}.arguments[{index}].value"),
                            ) {
                                return false;
                            }
                        }
                        CallArgument::InOut(place) => {
                            self.expression_item(
                                UnsupportedFeature::InOutArgument,
                                function,
                                expression,
                                &format!("{path}.arguments[{index}]"),
                            );
                            self.projected_place(
                                function,
                                Some(expression),
                                place,
                                expression.span,
                                &format!("{path}.arguments[{index}].place"),
                            );
                        }
                    }
                }
                if !type_arguments.is_empty() {
                    self.expression_item(
                        UnsupportedFeature::GenericCall,
                        function,
                        expression,
                        &format!("{path}.type_arguments"),
                    );
                }
                if !witnesses.is_empty() {
                    self.expression_item(
                        UnsupportedFeature::WitnessArguments,
                        function,
                        expression,
                        &format!("{path}.witnesses"),
                    );
                }
                let target_feature = match target {
                    CallTarget::Direct(_) | CallTarget::Inherent(_) => None,
                    CallTarget::StaticConcept { .. } => Some(UnsupportedFeature::StaticDispatch),
                    CallTarget::Dynamic { .. } => Some(UnsupportedFeature::DynamicDispatch),
                    CallTarget::Builtin(_) => Some(UnsupportedFeature::BuiltinCall),
                };
                if let Some(feature) = target_feature {
                    self.expression_item(feature, function, expression, &format!("{path}.target"));
                }
                expression.ty != Type::Never
            }
            ExprKind::MakeView { value, .. } => {
                if !self.visit_expr(function, value, &format!("{path}.value")) {
                    return false;
                }
                self.expression_item(UnsupportedFeature::View, function, expression, path);
                expression.ty != Type::Never
            }
            ExprKind::ReborrowView { owner, .. } => {
                self.expression_item(UnsupportedFeature::View, function, expression, path);
                self.projected_place(
                    function,
                    Some(expression),
                    owner,
                    expression.span,
                    &format!("{path}.owner"),
                );
                true
            }
            ExprKind::Await { task, .. } => {
                if !self.visit_expr(function, task, &format!("{path}.task")) {
                    return false;
                }
                self.expression_item(UnsupportedFeature::Suspension, function, expression, path);
                expression.ty != Type::Never
            }
            ExprKind::Sleep { milliseconds } => {
                if !self.visit_expr(function, milliseconds, &format!("{path}.milliseconds")) {
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
            ExprKind::WaitFd { descriptor, .. } => {
                if !self.visit_expr(function, descriptor, &format!("{path}.descriptor")) {
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
                if !self.visit_exprs(function, arguments, &format!("{path}.arguments")) {
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
        if continues && !is_supported_expression_type(&expression.ty) {
            self.expression_item(
                UnsupportedFeature::ExpressionType,
                function,
                expression,
                &format!("{path}.ty"),
            );
        }
        continues
    }

    fn visit_exprs(&mut self, function: FunctionId, expressions: &[mir::Expr], path: &str) -> bool {
        for (index, expression) in expressions.iter().enumerate() {
            if !self.visit_expr(function, expression, &format!("{path}[{index}]")) {
                return false;
            }
        }
        true
    }
}

#[derive(Clone, Debug, Default)]
struct EffectSummary {
    local_fault: bool,
    calls: BTreeSet<FunctionId>,
}

fn summarize_effects(function: &mir::Function) -> EffectSummary {
    let mut summary = EffectSummary::default();
    scan_effect_block(&function.body, &mut summary);
    summary
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
        StatementKind::ForRange {
            start, end, body, ..
        } => {
            if !scan_effect_expr(start, summary) || !scan_effect_expr(end, summary) {
                return false;
            }
            if scan_effect_block(body, summary) {
                // LCIR has no unchecked integer add. The source proof
                // `current < end` makes this checked edge unobservable, while
                // the IR still models it explicitly and therefore carries the
                // structural MAY_FAULT effect. This is correct route-selection
                // scaffolding, not the final production path: removing the
                // effect requires a general validated no-overflow LCIR form.
                summary.local_fault = true;
            }
            true
        }
        StatementKind::Assert { condition } => {
            let continues = scan_effect_expr(condition, summary);
            if continues {
                summary.local_fault = true;
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
                summary.local_fault = true;
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
                summary.local_fault = true;
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
            }
            expression.ty != Type::Never
        }
        ExprKind::MakeView { value, .. } => scan_effect_expr(value, summary),
        ExprKind::Await { task, .. } => scan_effect_expr(task, summary),
        ExprKind::Sleep { milliseconds } => scan_effect_expr(milliseconds, summary),
        ExprKind::WaitFd { descriptor, .. } => scan_effect_expr(descriptor, summary),
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
        StatementKind::Let { local, value } => {
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
        ExprKind::WaitFd { descriptor, .. } => scan_mutation_expr(descriptor, changed),
        ExprKind::TaskJoin { arguments, .. } => arguments
            .iter()
            .all(|argument| scan_mutation_expr(argument, changed)),
    };
    continues && expression.ty != Type::Never
}

fn solve_effects(
    summaries: &BTreeMap<FunctionId, EffectSummary>,
) -> Result<Vec<Option<Effects>>, LoweringError> {
    let slot_count = match summaries.keys().next_back().copied() {
        Some(function) => function_index(function)?.checked_add(1).ok_or_else(|| {
            LoweringError::ResourceLimit {
                code: ResourceLimitCode::ProgramTooLarge,
                message: "effect-plan slot count exceeds the host address space".to_owned(),
            }
        })?,
        None => 0,
    };
    let mut planned = allocated_slots(slot_count, false, "effect-plan membership")?;
    let mut incoming_counts = allocated_slots(slot_count, 0_usize, "reverse-call counts")?;
    let mut may_fault = allocated_slots(slot_count, false, "effect states")?;
    let mut pending = VecDeque::new();
    pending
        .try_reserve(slot_count)
        .map_err(|error| LoweringError::ResourceLimit {
            code: ResourceLimitCode::ProgramTooLarge,
            message: format!("cannot allocate effect worklist: {error}"),
        })?;

    for (function, summary) in summaries {
        let caller = function_index(*function)?;
        planned[caller] = true;
        if summary.local_fault {
            may_fault[caller] = true;
            pending.push_back(caller);
        }
    }
    for (function, summary) in summaries {
        for callee in &summary.calls {
            let callee_index = function_index(*callee)?;
            if !planned.get(callee_index).copied().unwrap_or(false) {
                return Err(LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    format!(
                        "reachable function #{} calls unplanned function #{}",
                        function.0, callee.0
                    ),
                ));
            }
            incoming_counts[callee_index] = incoming_counts[callee_index]
                .checked_add(1)
                .ok_or_else(|| LoweringError::ResourceLimit {
                    code: ResourceLimitCode::ProgramTooLarge,
                    message: format!("too many calls to function #{}", callee.0),
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
    for (function, summary) in summaries {
        let caller = function_index(*function)?;
        for callee in &summary.calls {
            let callee_index = function_index(*callee)?;
            reverse_calls[callee_index].push(caller);
        }
    }

    while let Some(callee) = pending.pop_front() {
        for caller in reverse_calls[callee].iter().copied() {
            if !may_fault[caller] {
                may_fault[caller] = true;
                pending.push_back(caller);
            }
        }
    }

    let mut effects = allocated_slots(slot_count, None, "effect plan")?;
    for function in summaries.keys().copied() {
        let index = function_index(function)?;
        effects[index] = Some(if may_fault[index] {
            Effects::MAY_FAULT
        } else {
            Effects::NONE
        });
    }
    Ok(effects)
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

fn function_index(function: FunctionId) -> Result<usize, LoweringError> {
    usize::try_from(function.0).map_err(|_| LoweringError::ResourceLimit {
        code: ResourceLimitCode::ProgramTooLarge,
        message: format!(
            "function #{} cannot be represented in the host address space",
            function.0
        ),
    })
}

fn effect_for(effects: &[Option<Effects>], function: FunctionId) -> Result<Effects, LoweringError> {
    effects
        .get(function_index(function)?)
        .copied()
        .flatten()
        .ok_or_else(|| {
            LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                format!("function #{} has no fixed-point effect", function.0),
            )
        })
}

#[derive(Clone)]
struct Flow {
    block: BlockId,
    env: BTreeMap<LocalId, ValueId>,
}

enum EvalFlow {
    Continue { flow: Flow, value: ValueId },
    Terminated,
}

enum StatementFlow {
    Continue(Flow),
    Terminated,
}

struct FunctionLowerer<'function, 'builder, 'plan> {
    source: &'function mir::Function,
    builder: FunctionBuilder<'builder>,
    instances: &'plan BTreeMap<FunctionId, InstanceId>,
    effects: &'plan [Option<Effects>],
    local_types: BTreeMap<LocalId, Type>,
    fault_block: Option<BlockId>,
}

impl<'function, 'builder, 'plan> FunctionLowerer<'function, 'builder, 'plan> {
    fn new(
        source: &'function mir::Function,
        builder: FunctionBuilder<'builder>,
        instances: &'plan BTreeMap<FunctionId, InstanceId>,
        effects: &'plan [Option<Effects>],
    ) -> Self {
        let local_types = source
            .params
            .iter()
            .chain(&source.locals)
            .map(|local| (local.id, local.ty.clone()))
            .collect();
        Self {
            source,
            builder,
            instances,
            effects,
            local_types,
            fault_block: None,
        }
    }

    fn lower(mut self) -> Result<(), LoweringError> {
        let entry = self.create_block()?;
        self.builder.set_entry(entry).map_err(LoweringError::from)?;
        let mut env = BTreeMap::new();
        for parameter in &self.source.params {
            let ty = self.type_id(&parameter.ty)?;
            let value = self
                .builder
                .append_block_parameter(entry, ty)
                .map_err(LoweringError::from)?;
            env.insert(parameter.id, value);
        }
        let flow = Flow { block: entry, env };
        match self.lower_scoped_block(flow, &self.source.body)? {
            EvalFlow::Continue { flow, value } => self.terminate(
                flow.block,
                TerminatorKind::Return(value),
                self.block_origin(&self.source.body),
            ),
            EvalFlow::Terminated => Ok(()),
        }
    }

    fn type_id(&self, ty: &Type) -> Result<ValueTypeId, LoweringError> {
        self.builder.representations().type_id(ty).ok_or_else(|| {
            LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                format!("classified scalar type {ty:?} has no LCIR type"),
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
        let origin = Origin {
            source_function: self.source.id,
            expression: None,
            span: self.source.span,
        };
        self.terminate(block, TerminatorKind::ResumeFault, origin)?;
        self.fault_block = Some(block);
        Ok(block)
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
        // MIR LocalId values are function-scoped. The checked MIR dataflow is
        // the authority for whether a local is available after a nested block;
        // source lexical scopes are no longer present at this layer.
        self.lower_block(flow, block)
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
                    flow.env.insert(*local, value);
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
                EvalFlow::Continue { mut flow, value } => {
                    if !place.projection.is_empty() {
                        return Err(self.unsupported_reached("projected assignment"));
                    }
                    flow.env.insert(place.local, value);
                    Ok(StatementFlow::Continue(flow))
                }
                EvalFlow::Terminated => Ok(StatementFlow::Terminated),
            },
            StatementKind::LetTuple { value, .. } => match self.lower_expr(flow, value)? {
                EvalFlow::Continue { .. } => Err(self.unsupported_reached("tuple binding")),
                EvalFlow::Terminated => Ok(StatementFlow::Terminated),
            },
            StatementKind::Assert { condition } => match self.lower_expr(flow, condition)? {
                EvalFlow::Continue { .. } => Err(self.unsupported_reached("assertion cleanup")),
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
                        self.terminate(flow.block, TerminatorKind::Return(value), origin)?;
                        Ok(StatementFlow::Terminated)
                    }
                    EvalFlow::Terminated => Ok(StatementFlow::Terminated),
                }
            }
            StatementKind::Defer(_) => Err(self.unsupported_reached("deferred cleanup")),
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
                    mir::Constant::Text(_) => {
                        return Err(self.unsupported_reached("text constant"));
                    }
                };
                self.constant(flow, constant, &expression.ty, origin)
            }
            ExprKind::Copy(place) | ExprKind::Move(place) => {
                if !place.projection.is_empty() {
                    return Err(self.unsupported_reached("projected place read"));
                }
                let mut flow = flow;
                let value = flow.env.get(&place.local).copied().ok_or_else(|| {
                    LoweringError::defect(
                        LoweringDefectCode::InconsistentPlan,
                        format!(
                            "function #{} reads unavailable local #{} at expression #{}",
                            self.source.id.0, place.local.0, expression.id.0
                        ),
                    )
                })?;
                if matches!(expression.kind, ExprKind::Move(_)) {
                    flow.env.remove(&place.local);
                }
                Ok(EvalFlow::Continue { flow, value })
            }
            ExprKind::Unary(operator, operand) => {
                let EvalFlow::Continue { flow, value } = self.lower_expr(flow, operand)? else {
                    return Ok(EvalFlow::Terminated);
                };
                match (operator, &operand.ty) {
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
            ExprKind::Tuple(values) => self.lower_unsupported_values(flow, values, "tuple value"),
            ExprKind::List(values) => self.lower_unsupported_values(flow, values, "list value"),
            ExprKind::Match { scrutinee, .. } => {
                self.lower_unsupported_operand(flow, scrutinee, "pattern match")
            }
            ExprKind::Record { fields, .. } => {
                self.lower_unsupported_values(flow, fields, "record value")
            }
            ExprKind::Variant { payload, .. } => {
                self.lower_unsupported_values(flow, payload, "variant value")
            }
            ExprKind::Refine { value, .. } | ExprKind::Unrefine(value) => {
                self.lower_unsupported_operand(flow, value, "refined value")
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
            ExprKind::WaitFd { descriptor, .. } => {
                self.lower_unsupported_operand(flow, descriptor, "fd wait")
            }
            ExprKind::TaskJoin { arguments, .. } => {
                self.lower_unsupported_values(flow, arguments, "task join")
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
        if *operand_type == Type::Int
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
        if *operand_type == Type::Float
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
        let kind = match operand_type {
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
        let fault = self.fault_block()?;
        self.terminate(
            flow.block,
            TerminatorKind::CheckedIntNegate {
                value,
                normal: ResultTarget::new(normal, []),
                fault: UnwindTarget::new(fault, []),
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
        let fault = self.fault_block()?;
        self.terminate(
            flow.block,
            TerminatorKind::CheckedIntBinary {
                op,
                left,
                right,
                normal: ResultTarget::new(normal, []),
                fault: UnwindTarget::new(fault, []),
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
                env: flow.env.clone(),
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

        let locals = flow
            .env
            .keys()
            .copied()
            .filter(|local| right_flow.env.contains_key(local))
            .collect::<Vec<_>>();
        let join = self.create_block()?;
        let result_varies = condition != right_value;
        let result = if result_varies {
            self.builder
                .append_block_parameter(join, self.type_id(&Type::Bool)?)
                .map_err(LoweringError::from)?
        } else {
            condition
        };
        let mut env = BTreeMap::new();
        let mut varying_locals = Vec::new();
        for local in locals {
            let skipped = *flow.env.get(&local).ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    format!("short-circuit skip path lost local #{}", local.0),
                )
            })?;
            let evaluated = *right_flow.env.get(&local).ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    format!("short-circuit RHS lost local #{}", local.0),
                )
            })?;
            if skipped == evaluated {
                env.insert(local, skipped);
            } else {
                let parameter = self
                    .builder
                    .append_block_parameter(join, self.local_type(local)?)
                    .map_err(LoweringError::from)?;
                env.insert(local, parameter);
                varying_locals.push(local);
            }
        }

        let mut skip_arguments =
            Vec::with_capacity(usize::from(result_varies) + varying_locals.len());
        let mut right_arguments =
            Vec::with_capacity(usize::from(result_varies) + varying_locals.len());
        if result_varies {
            skip_arguments.push(condition);
            right_arguments.push(right_value);
        }
        for local in &varying_locals {
            skip_arguments.push(*flow.env.get(local).ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    format!("short-circuit skip argument lost local #{}", local.0),
                )
            })?);
            right_arguments.push(*right_flow.env.get(local).ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    format!("short-circuit RHS argument lost local #{}", local.0),
                )
            })?);
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
                env: flow.env.clone(),
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
            &expression.ty,
            self.expression_origin(expression),
        )
    }

    fn merge_evaluations<const N: usize>(
        &mut self,
        alternatives: [EvalFlow; N],
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
        let Some((first_flow, first_value)) = continuing.first() else {
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

        // A move on only one incoming path makes that local unavailable after
        // the join. Checked MIR prevents a later read of such a local, so the
        // SSA environment carries precisely the intersection of available
        // locals instead of inventing a value for the moved path.
        let locals = first_flow
            .env
            .keys()
            .copied()
            .filter(|local| {
                continuing
                    .iter()
                    .all(|(flow, _)| flow.env.contains_key(local))
            })
            .collect::<Vec<_>>();

        let join = self.create_block()?;
        let result_varies = continuing.iter().any(|(_, value)| value != first_value);
        let result = if result_varies {
            self.builder
                .append_block_parameter(join, self.type_id(result_type)?)
                .map_err(LoweringError::from)?
        } else {
            *first_value
        };
        let mut env = BTreeMap::new();
        let mut varying_locals = Vec::new();
        for local in &locals {
            let first = *first_flow.env.get(local).ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    format!("first join alternative has no local #{}", local.0),
                )
            })?;
            if continuing
                .iter()
                .all(|(flow, _)| flow.env.get(local) == Some(&first))
            {
                // A value defined before the branch already dominates the
                // join and does not need an identity block parameter.
                env.insert(*local, first);
            } else {
                let parameter = self
                    .builder
                    .append_block_parameter(join, self.local_type(*local)?)
                    .map_err(LoweringError::from)?;
                env.insert(*local, parameter);
                varying_locals.push(*local);
            }
        }
        for (flow, value) in continuing {
            let mut arguments =
                Vec::with_capacity(usize::from(result_varies) + varying_locals.len());
            if result_varies {
                arguments.push(value);
            }
            for local in &varying_locals {
                arguments.push(*flow.env.get(local).ok_or_else(|| {
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
        let outer = flow
            .env
            .keys()
            .copied()
            .filter(|candidate| *candidate != local)
            .collect::<Vec<_>>();
        let mutations = continuing_mutations(body).unwrap_or_default();
        let carried = outer
            .iter()
            .copied()
            .filter(|candidate| mutations.contains(candidate))
            .collect::<Vec<_>>();
        let header = self.create_block()?;
        let body_block = self.create_block()?;
        let exit = self.create_block()?;
        let integer = self.type_id(&Type::Int)?;
        let current = self
            .builder
            .append_block_parameter(header, integer)
            .map_err(LoweringError::from)?;
        let mut header_env = BTreeMap::new();
        let mut preheader_arguments = vec![start];
        for outer_local in &outer {
            let incoming = *flow.env.get(outer_local).ok_or_else(|| {
                LoweringError::defect(
                    LoweringDefectCode::InconsistentPlan,
                    format!("range lost outer local #{}", outer_local.0),
                )
            })?;
            if mutations.contains(outer_local) {
                let parameter = self
                    .builder
                    .append_block_parameter(header, self.local_type(*outer_local)?)
                    .map_err(LoweringError::from)?;
                header_env.insert(*outer_local, parameter);
                preheader_arguments.push(incoming);
            } else {
                // Values defined before the loop dominate its header and do
                // not need identity backedge arguments.
                header_env.insert(*outer_local, incoming);
            }
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
                env: header_env.clone(),
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

        let mut body_env = header_env.clone();
        body_env.insert(local, current);
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
            let (increment_flow, one) =
                match self.constant(body_flow, Constant::Int(1), &Type::Int, origin)? {
                    EvalFlow::Continue { flow, value } => (flow, value),
                    EvalFlow::Terminated => {
                        return Err(LoweringError::defect(
                            LoweringDefectCode::Builder,
                            "range increment constant unexpectedly terminated",
                        ));
                    }
                };
            let (next_flow, next) = match self.lower_checked_binary(
                increment_flow,
                CheckedIntBinaryOp::Add,
                current,
                one,
                origin,
            )? {
                EvalFlow::Continue { flow, value } => (flow, value),
                EvalFlow::Terminated => {
                    return Err(LoweringError::defect(
                        LoweringDefectCode::Builder,
                        "range increment instruction unexpectedly terminated",
                    ));
                }
            };
            let mut backedge_arguments = vec![next];
            for outer_local in &carried {
                backedge_arguments.push(*next_flow.env.get(outer_local).ok_or_else(|| {
                    LoweringError::defect(
                        LoweringDefectCode::InconsistentPlan,
                        format!("range body lost outer local #{}", outer_local.0),
                    )
                })?);
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

    fn lower_call(
        &mut self,
        mut flow: Flow,
        target: &CallTarget,
        type_arguments: &[Type],
        arguments: &[CallArgument],
        witnesses: &[mir::WitnessRef],
        expression: &mir::Expr,
    ) -> Result<EvalFlow, LoweringError> {
        let mut lowered_arguments = Vec::with_capacity(arguments.len());
        for argument in arguments {
            let CallArgument::Value(argument) = argument else {
                return Err(self.unsupported_reached("inout call argument"));
            };
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
        if !type_arguments.is_empty() || !witnesses.is_empty() {
            return Err(self.unsupported_reached("generic or witnessed call"));
        }
        let callee = match target {
            CallTarget::Direct(callee) | CallTarget::Inherent(callee) => *callee,
            _ => return Err(self.unsupported_reached("non-direct call")),
        };
        let instance = self.instances.get(&callee).copied().ok_or_else(|| {
            LoweringError::defect(
                LoweringDefectCode::InconsistentPlan,
                format!("call target #{} has no LCIR instance", callee.0),
            )
        })?;
        let effect = effect_for(self.effects, callee)?;
        let origin = self.expression_origin(expression);
        let result_type = self.type_id(&expression.ty)?;
        if effect == Effects::NONE {
            return self.one_instruction(
                flow,
                InstructionKind::DirectCall {
                    callee: instance,
                    arguments: lowered_arguments.into_boxed_slice(),
                },
                result_type,
                origin,
            );
        }
        let normal = self.create_block()?;
        let result = self
            .builder
            .append_block_parameter(normal, result_type)
            .map_err(LoweringError::from)?;
        let fault = self.fault_block()?;
        self.terminate(
            flow.block,
            TerminatorKind::Invoke {
                callee: instance,
                arguments: lowered_arguments.into_boxed_slice(),
                normal: ResultTarget::new(normal, []),
                unwind: UnwindTarget::new(fault, []),
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
        let mut summaries = BTreeMap::new();
        for raw in 0..FUNCTION_COUNT {
            let mut summary = EffectSummary {
                local_fault: raw == 0,
                ..EffectSummary::default()
            };
            if raw != 0 {
                summary.calls.insert(FunctionId(raw - 1));
            }
            summaries.insert(FunctionId(raw), summary);
        }

        let effects = solve_effects(&summaries).expect("long-chain effects must solve");
        for raw in 0..FUNCTION_COUNT {
            assert_eq!(
                effect_for(&effects, FunctionId(raw)).expect("planned function"),
                Effects::MAY_FAULT
            );
        }
    }

    #[test]
    fn effect_solver_propagates_around_a_recursive_scc() {
        let summaries = BTreeMap::from([
            (
                FunctionId(0),
                EffectSummary {
                    calls: BTreeSet::from([FunctionId(1)]),
                    ..EffectSummary::default()
                },
            ),
            (
                FunctionId(1),
                EffectSummary {
                    calls: BTreeSet::from([FunctionId(2)]),
                    ..EffectSummary::default()
                },
            ),
            (
                FunctionId(2),
                EffectSummary {
                    local_fault: true,
                    calls: BTreeSet::from([FunctionId(0)]),
                },
            ),
            (FunctionId(3), EffectSummary::default()),
        ]);

        let effects = solve_effects(&summaries).expect("recursive effects must solve");
        for raw in 0..3 {
            assert_eq!(
                effect_for(&effects, FunctionId(raw)).expect("planned SCC member"),
                Effects::MAY_FAULT
            );
        }
        assert_eq!(
            effect_for(&effects, FunctionId(3)).expect("planned pure function"),
            Effects::NONE
        );
    }

    #[test]
    fn effect_solver_rejects_an_unplanned_callee_as_a_defect() {
        let summaries = BTreeMap::from([(
            FunctionId(0),
            EffectSummary {
                calls: BTreeSet::from([FunctionId(1)]),
                ..EffectSummary::default()
            },
        )]);

        let error = solve_effects(&summaries).expect_err("unplanned callee must be rejected");
        assert_eq!(
            error.code(),
            LoweringErrorCode::Defect(LoweringDefectCode::InconsistentPlan)
        );
    }
}
