//! Closed-world integer range facts used only to remove proved runtime checks.
//!
//! This compiler-private analysis is deliberately fail-closed. It propagates value facts through
//! direct calls and records a proof only when every visit to an arithmetic site is safe. Counted
//! loops recognize translation recurrences from one symbolic iteration and close their bounds with
//! `i128` arithmetic. Unknown aliases, mutation, dispatch, suspension, cleanup, or witness entry
//! immediately lose the affected facts.
//!
//! The plan is not serialized: requirements and emission query the same stable
//! `(FunctionId, ExprId)` site in one immutable MIR program.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use loom_codegen_ir::{ReachableSourceGraph, SourceRoots};
use loom_mir::{
    BinaryOp, Block, Builtin, CallArgument, CallTarget, Constant, Expr, ExprId, ExprKind, Function,
    FunctionId, LocalId, Pattern, Place, Program, StatementKind, Type, TypeDefKind, TypeId,
    UnaryOp, VariantId,
};

const CONTEXT_WIDEN_LIMIT: usize = 64;
const LOOP_FIXPOINT_LIMIT: usize = 12;
const TYPE_EXPANSION_LIMIT: usize = 16;
const ASSUMED_RECURSION_LIMIT: i64 = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct IntRange {
    lower: i64,
    upper: i64,
}

impl IntRange {
    const FULL: Self = Self {
        lower: i64::MIN,
        upper: i64::MAX,
    };

    const fn exact(value: i64) -> Self {
        Self {
            lower: value,
            upper: value,
        }
    }

    const fn new(lower: i64, upper: i64) -> Self {
        Self { lower, upper }
    }

    const fn join(self, other: Self) -> Self {
        Self {
            lower: if self.lower < other.lower {
                self.lower
            } else {
                other.lower
            },
            upper: if self.upper > other.upper {
                self.upper
            } else {
                other.upper
            },
        }
    }

    fn intersect(self, other: Self) -> Self {
        let lower = self.lower.max(other.lower);
        let upper = self.upper.min(other.upper);
        if lower <= upper {
            Self { lower, upper }
        } else {
            // Never derive facts from an abstractly impossible branch.
            self
        }
    }

    const fn exact_value(self) -> Option<i64> {
        if self.lower == self.upper {
            Some(self.lower)
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct PlaceSite {
    local: LocalId,
    projection: Vec<u32>,
}

impl From<&Place> for PlaceSite {
    fn from(place: &Place) -> Self {
        Self {
            local: place.local,
            projection: place.projection.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum OriginKind {
    Integer,
    ListLength,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct OriginSite {
    function: FunctionId,
    place: PlaceSite,
    kind: OriginKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AffineForm {
    terms: BTreeMap<OriginSite, i128>,
    residual: IntRange,
}

impl AffineForm {
    fn range(range: IntRange) -> Self {
        Self {
            terms: BTreeMap::new(),
            residual: range,
        }
    }

    fn origin(origin: OriginSite) -> Self {
        Self {
            terms: BTreeMap::from([(origin, 1)]),
            residual: IntRange::exact(0),
        }
    }

    fn combine(self, other: Self, subtract: bool) -> Option<Self> {
        let mut terms = self.terms;
        for (origin, coefficient) in other.terms {
            let coefficient = if subtract {
                coefficient.checked_neg()?
            } else {
                coefficient
            };
            let current = terms.get(&origin).copied().unwrap_or(0);
            let coefficient = current.checked_add(coefficient)?;
            if coefficient == 0 {
                terms.remove(&origin);
            } else {
                terms.insert(origin, coefficient);
            }
        }
        let operator = if subtract {
            BinaryOp::Subtract
        } else {
            BinaryOp::Add
        };
        let (residual, safe) = checked_binary_range(operator, self.residual, other.residual);
        safe.then_some(Self { terms, residual })
    }

    fn negate(self) -> Option<Self> {
        let mut terms = BTreeMap::new();
        for (origin, coefficient) in self.terms {
            terms.insert(origin, coefficient.checked_neg()?);
        }
        let lower = i128::from(self.residual.upper).checked_neg()?;
        let upper = i128::from(self.residual.lower).checked_neg()?;
        Some(Self {
            terms,
            residual: checked_i128_range(lower, upper)?,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct IntFact {
    range: IntRange,
    affine: Option<AffineForm>,
}

impl IntFact {
    fn range(range: IntRange) -> Self {
        Self {
            range,
            affine: Some(AffineForm::range(range)),
        }
    }

    fn dependent_range(range: IntRange) -> Self {
        Self {
            range,
            affine: None,
        }
    }

    fn exact(value: i64) -> Self {
        Self::range(IntRange::exact(value))
    }

    fn origin(range: IntRange, origin: OriginSite) -> Self {
        Self {
            range,
            affine: Some(AffineForm::origin(origin)),
        }
    }

    fn join(&self, other: &Self) -> Self {
        let affine = match (&self.affine, &other.affine) {
            (Some(left), Some(right)) if left.terms == right.terms => Some(AffineForm {
                terms: left.terms.clone(),
                residual: left.residual.join(right.residual),
            }),
            _ => None,
        };
        Self {
            range: self.range.join(other.range),
            affine,
        }
    }

    fn without_affine(&self) -> Self {
        Self::range(self.range)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ListFacts {
    /// Envelope of every element that can exist. `None` proves the list empty.
    element: Option<IntRange>,
    /// True only when `element` is closed over every state represented here.
    element_stable: bool,
    length: IntFact,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EnumFacts {
    ty: TypeId,
    variants: BTreeMap<VariantId, Vec<AbstractValue>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum AbstractValue {
    Top,
    Int(IntFact),
    Record {
        ty: TypeId,
        fields: Vec<AbstractValue>,
    },
    IntList(ListFacts),
    Enum(EnumFacts),
}

impl AbstractValue {
    fn int_range(&self) -> IntRange {
        match self {
            Self::Int(value) => value.range,
            _ => IntRange::FULL,
        }
    }

    fn join(&self, other: &Self) -> Self {
        match (self, other) {
            (Self::Int(left), Self::Int(right)) => Self::Int(left.join(right)),
            (
                Self::Record {
                    ty: left_ty,
                    fields: left,
                },
                Self::Record {
                    ty: right_ty,
                    fields: right,
                },
            ) if left_ty == right_ty && left.len() == right.len() => Self::Record {
                ty: *left_ty,
                fields: left
                    .iter()
                    .zip(right)
                    .map(|(left, right)| left.join(right))
                    .collect(),
            },
            (Self::IntList(left), Self::IntList(right)) => Self::IntList(ListFacts {
                element: match (left.element, right.element) {
                    (Some(left), Some(right)) => Some(left.join(right)),
                    (Some(value), None) | (None, Some(value)) => Some(value),
                    (None, None) => None,
                },
                element_stable: left.element_stable && right.element_stable,
                length: left.length.join(&right.length),
            }),
            (Self::Enum(left), Self::Enum(right)) if left.ty == right.ty => {
                let mut variants = BTreeMap::new();
                for variant in left.variants.keys().chain(right.variants.keys()).copied() {
                    let payload = match (left.variants.get(&variant), right.variants.get(&variant))
                    {
                        (Some(left), Some(right)) if left.len() == right.len() => left
                            .iter()
                            .zip(right)
                            .map(|(left, right)| left.join(right))
                            .collect(),
                        (Some(value), None) | (None, Some(value)) => value.clone(),
                        _ => return Self::Top,
                    };
                    variants.insert(variant, payload);
                }
                Self::Enum(EnumFacts {
                    ty: left.ty,
                    variants,
                })
            }
            _ => Self::Top,
        }
    }

    fn without_affine(&self) -> Self {
        match self {
            Self::Int(value) => Self::Int(value.without_affine()),
            Self::Record { ty, fields } => Self::Record {
                ty: *ty,
                fields: fields.iter().map(Self::without_affine).collect(),
            },
            Self::IntList(list) => Self::IntList(ListFacts {
                element: list.element,
                element_stable: list.element_stable,
                length: list.length.without_affine(),
            }),
            Self::Enum(value) => Self::Enum(EnumFacts {
                ty: value.ty,
                variants: value
                    .variants
                    .iter()
                    .map(|(variant, payload)| {
                        (*variant, payload.iter().map(Self::without_affine).collect())
                    })
                    .collect(),
            }),
            Self::Top => Self::Top,
        }
    }
}

type RangeEnv = BTreeMap<LocalId, AbstractValue>;

#[derive(Clone, Debug)]
struct FunctionContext {
    reached: bool,
    parameters: Vec<Option<AbstractValue>>,
}

impl FunctionContext {
    fn empty(function: &Function) -> Self {
        Self {
            reached: false,
            parameters: function.params.iter().map(|_| None).collect(),
        }
    }
}

#[derive(Clone, Debug)]
struct CallObservation {
    function: FunctionId,
    arguments: Vec<AbstractValue>,
    site: Option<ExprSite>,
}

#[derive(Clone, Debug)]
struct CallSummary {
    result: AbstractValue,
    parameters: Vec<AbstractValue>,
}

/// Selects which compiler-private body is being analyzed or emitted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NativeBodyMode {
    Checked,
    Assumed,
}

/// A dense, exhaustively checked non-negative input domain for one pure,
/// well-founded recursive Int function.
#[derive(Clone, Debug)]
pub(crate) struct AssumedIntFunctionPlan {
    upper: i64,
    exact_results: Vec<i64>,
    arithmetic: BTreeSet<ExprId>,
    recursive_calls: BTreeSet<ExprId>,
}

impl AssumedIntFunctionPlan {
    #[must_use]
    pub(crate) const fn upper(&self) -> i64 {
        self.upper
    }

    #[must_use]
    pub(crate) fn exact_result(&self, input: i64) -> Option<i64> {
        usize::try_from(input)
            .ok()
            .and_then(|input| self.exact_results.get(input).copied())
    }

    fn contains(&self, range: IntRange) -> bool {
        range.lower >= 0 && range.upper <= self.upper && self.exact_result(range.upper).is_some()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectCallFact {
    callee: FunctionId,
    argument: IntRange,
}

/// Proven checked-Int expressions and compiler-private assumed bodies for one
/// immutable MIR program.
#[derive(Clone, Debug, Default)]
pub(crate) struct NativeIntRangePlan {
    arithmetic: BTreeSet<ExprSite>,
    assumed: BTreeMap<FunctionId, AssumedIntFunctionPlan>,
    assumed_call_sites: BTreeSet<ExprSite>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ExprSite {
    function: FunctionId,
    expression: ExprId,
}

impl NativeIntRangePlan {
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub(crate) fn analyze(
        program: &Program,
        reachable: &ReachableSourceGraph,
        roots: &SourceRoots,
    ) -> Self {
        let mut contexts = reachable
            .functions
            .iter()
            .filter_map(|id| {
                program
                    .function(*id)
                    .map(|function| (*id, FunctionContext::empty(function)))
            })
            .collect::<BTreeMap<_, _>>();
        let mut queue = VecDeque::new();
        let mut context_updates = BTreeMap::<FunctionId, usize>::new();

        for root in roots.functions() {
            let Some(function) = program.function(*root) else {
                continue;
            };
            let context = contexts
                .entry(*root)
                .or_insert_with(|| FunctionContext::empty(function));
            set_unknown_entry(program, function, context);
            queue.push_back(*root);
        }

        // These entry edges are not described by the synchronous direct-call graph.
        let mut unknown_entries = reachable
            .functions
            .iter()
            .filter(|id| {
                program.function(**id).is_some_and(|function| {
                    function.is_async
                        || !function.witness_params.is_empty()
                        || block_contains_defer(&function.body)
                })
            })
            .copied()
            .collect::<BTreeSet<_>>();
        for (witness, requirements) in &reachable.witness_methods {
            let Some(witness) = program.witness(*witness) else {
                continue;
            };
            unknown_entries.extend(
                requirements
                    .iter()
                    .filter_map(|requirement| witness.methods.get(requirement).copied()),
            );
        }
        for function_id in unknown_entries {
            let Some(function) = program.function(function_id) else {
                continue;
            };
            let context = contexts
                .entry(function_id)
                .or_insert_with(|| FunctionContext::empty(function));
            set_unknown_entry(program, function, context);
            queue.push_back(function_id);
        }

        while let Some(function_id) = queue.pop_front() {
            let Some(function) = program.function(function_id) else {
                continue;
            };
            let Some(context) = contexts.get(&function_id).cloned() else {
                continue;
            };
            let mut observations = Vec::new();
            let mut ignored_proofs = BTreeMap::new();
            let mut analyzer = FunctionAnalyzer {
                program,
                function_id,
                observations: &mut observations,
                proof_visits: &mut ignored_proofs,
                record_proofs: false,
                call_stack: BTreeSet::from([function_id]),
                conservative_calls: block_contains_defer(&function.body)
                    || block_contains_nested_loop(&function.body),
            };
            analyzer.scan(function, &context);

            for observation in observations {
                let Some(callee) = program.function(observation.function) else {
                    continue;
                };
                let Some(target) = contexts.get_mut(&observation.function) else {
                    continue;
                };
                let mut changed = !target.reached;
                target.reached = true;
                let update_count = context_updates.entry(observation.function).or_default();
                for ((slot, incoming), parameter) in target
                    .parameters
                    .iter_mut()
                    .zip(observation.arguments)
                    .zip(&callee.params)
                {
                    let incoming = normalize_for_type(program, &parameter.ty, incoming);
                    let mut joined = slot
                        .as_ref()
                        .map_or_else(|| incoming.clone(), |current| current.join(&incoming));
                    if *update_count >= CONTEXT_WIDEN_LIMIT
                        && slot.as_ref().is_some_and(|current| current != &joined)
                    {
                        joined = unknown_value(program, &parameter.ty, 0);
                    }
                    changed |= slot.as_ref() != Some(&joined);
                    *slot = Some(joined);
                }
                if changed {
                    *update_count = update_count.saturating_add(1);
                    queue.push_back(observation.function);
                }
            }
        }

        let mut proof_visits = BTreeMap::<ExprSite, bool>::new();
        let mut direct_calls = BTreeMap::<ExprSite, DirectCallFact>::new();
        for (function_id, context) in &contexts {
            if !context.reached {
                continue;
            }
            let Some(function) = program.function(*function_id) else {
                continue;
            };
            if function.is_async
                || !function.witness_params.is_empty()
                || block_contains_defer(&function.body)
                || block_contains_nested_loop(&function.body)
            {
                continue;
            }
            let mut observations = Vec::new();
            let mut analyzer = FunctionAnalyzer {
                program,
                function_id: *function_id,
                observations: &mut observations,
                proof_visits: &mut proof_visits,
                record_proofs: true,
                call_stack: BTreeSet::from([*function_id]),
                conservative_calls: false,
            };
            analyzer.scan(function, context);
            for observation in observations {
                let (Some(site), [AbstractValue::Int(argument)]) =
                    (observation.site, observation.arguments.as_slice())
                else {
                    continue;
                };
                let incoming = DirectCallFact {
                    callee: observation.function,
                    argument: argument.range,
                };
                direct_calls
                    .entry(site)
                    .and_modify(|current| {
                        if current.callee == incoming.callee {
                            current.argument = current.argument.join(incoming.argument);
                        } else {
                            current.argument = IntRange::FULL;
                        }
                    })
                    .or_insert(incoming);
            }
        }

        let assumed = reachable
            .functions
            .iter()
            .filter_map(|function| {
                program
                    .function(*function)
                    .and_then(analyze_assumed_int_function)
                    .map(|plan| (*function, plan))
            })
            .collect::<BTreeMap<_, _>>();
        let assumed_call_sites = direct_calls
            .into_iter()
            .filter_map(|(site, call)| {
                assumed
                    .get(&call.callee)
                    .is_some_and(|plan| plan.contains(call.argument))
                    .then_some(site)
            })
            .collect();

        Self {
            arithmetic: proof_visits
                .into_iter()
                .filter_map(|(expression, safe)| safe.then_some(expression))
                .collect(),
            assumed,
            assumed_call_sites,
        }
    }

    #[must_use]
    pub(crate) fn proves(&self, function: FunctionId, expression: &Expr) -> bool {
        self.arithmetic.contains(&ExprSite {
            function,
            expression: expression.id,
        })
    }

    #[must_use]
    pub(crate) fn proves_for(
        &self,
        mode: NativeBodyMode,
        function: FunctionId,
        expression: &Expr,
    ) -> bool {
        match mode {
            NativeBodyMode::Checked => self.proves(function, expression),
            NativeBodyMode::Assumed => self
                .assumed
                .get(&function)
                .is_some_and(|plan| plan.arithmetic.contains(&expression.id)),
        }
    }

    #[must_use]
    pub(crate) fn assumption(&self, function: FunctionId) -> Option<&AssumedIntFunctionPlan> {
        self.assumed.get(&function)
    }

    #[must_use]
    pub(crate) fn uses_assumed_call(
        &self,
        mode: NativeBodyMode,
        source_function: FunctionId,
        expression: &Expr,
        target_function: FunctionId,
    ) -> bool {
        if self.assumption(target_function).is_none() {
            return false;
        }
        match mode {
            NativeBodyMode::Checked => {
                source_function != target_function
                    && self.assumed_call_sites.contains(&ExprSite {
                        function: source_function,
                        expression: expression.id,
                    })
            }
            NativeBodyMode::Assumed => {
                source_function == target_function
                    && self
                        .assumed
                        .get(&source_function)
                        .is_some_and(|plan| plan.recursive_calls.contains(&expression.id))
            }
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct AssumedSyntax {
    arithmetic: BTreeSet<ExprId>,
    recursive_calls: BTreeSet<ExprId>,
}

#[derive(Clone, Debug, Default)]
struct ExactProof {
    arithmetic: BTreeSet<ExprId>,
    recursive_calls: BTreeSet<ExprId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExactValue {
    Unit,
    Bool(bool),
    Int(i64),
}

fn analyze_assumed_int_function(function: &Function) -> Option<AssumedIntFunctionPlan> {
    if function.is_async
        || function.type_parameters != 0
        || !function.witness_params.is_empty()
        || function.receiver.is_some()
        || function.params.len() != 1
        || function.params[0].ty != Type::Int
        || function.params[0].mutable
        || function.return_ty != Type::Int
        || function.call_plan.receiver_invariant.is_some()
        || !function.call_plan.requires.is_empty()
        || !function.call_plan.ensures.is_empty()
        || function
            .locals
            .iter()
            .any(|local| local.mutable || !is_exact_scalar_type(&local.ty))
    {
        return None;
    }

    let syntax = assumed_syntax(function)?;
    if syntax.recursive_calls.is_empty() {
        return None;
    }

    let mut exact_results = Vec::new();
    let mut proof = ExactProof::default();
    for input in 0..=ASSUMED_RECURSION_LIMIT {
        let mut trial = proof.clone();
        let evaluated = ExactEvaluator {
            function,
            input,
            exact_results: &exact_results,
            proof: &mut trial,
        }
        .evaluate();
        let Some(result) = evaluated else {
            break;
        };
        let ExactValue::Int(result) = result else {
            return None;
        };
        exact_results.push(result);
        proof = trial;
    }

    finish_assumed_plan(&syntax, exact_results, proof)
}

fn finish_assumed_plan(
    syntax: &AssumedSyntax,
    exact_results: Vec<i64>,
    proof: ExactProof,
) -> Option<AssumedIntFunctionPlan> {
    if exact_results.len() < 3
        || proof.arithmetic != syntax.arithmetic
        || proof.recursive_calls != syntax.recursive_calls
    {
        return None;
    }
    let upper = i64::try_from(exact_results.len().checked_sub(1)?).ok()?;
    Some(AssumedIntFunctionPlan {
        upper,
        exact_results,
        arithmetic: proof.arithmetic,
        recursive_calls: proof.recursive_calls,
    })
}

fn assumed_syntax(function: &Function) -> Option<AssumedSyntax> {
    let mut syntax = AssumedSyntax::default();
    collect_assumed_block(function, &function.body, &mut syntax)?;
    Some(syntax)
}

fn collect_assumed_block(
    function: &Function,
    block: &Block,
    syntax: &mut AssumedSyntax,
) -> Option<()> {
    for statement in &block.statements {
        let StatementKind::Let { value, .. } = &statement.kind else {
            return None;
        };
        collect_assumed_expr(function, value, syntax)?;
    }
    collect_assumed_expr(function, block.tail.as_deref()?, syntax)
}

fn collect_assumed_expr(
    function: &Function,
    expression: &Expr,
    syntax: &mut AssumedSyntax,
) -> Option<()> {
    if expression.id == ExprId::UNASSIGNED || !is_exact_scalar_type(&expression.ty) {
        return None;
    }
    match &expression.kind {
        ExprKind::Constant(Constant::Unit | Constant::Bool(_) | Constant::Int(_)) => Some(()),
        ExprKind::Copy(place) | ExprKind::Move(place) if place.projection.is_empty() => Some(()),
        ExprKind::Unary(operator, value) => {
            collect_assumed_expr(function, value, syntax)?;
            if *operator == UnaryOp::Negate && expression.ty == Type::Int {
                syntax.arithmetic.insert(expression.id);
            }
            Some(())
        }
        ExprKind::Binary(operator, left, right) => {
            collect_assumed_expr(function, left, syntax)?;
            collect_assumed_expr(function, right, syntax)?;
            if matches!(
                operator,
                BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide
            ) && expression.ty == Type::Int
            {
                syntax.arithmetic.insert(expression.id);
            }
            Some(())
        }
        ExprKind::Block(block) => collect_assumed_block(function, block, syntax),
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            collect_assumed_expr(function, condition, syntax)?;
            collect_assumed_block(function, then_branch, syntax)?;
            collect_assumed_block(function, else_branch, syntax)
        }
        ExprKind::Call {
            target: CallTarget::Direct(callee),
            type_arguments,
            arguments,
            witnesses,
        } if *callee == function.id && type_arguments.is_empty() && witnesses.is_empty() => {
            let [CallArgument::Value(argument)] = arguments.as_slice() else {
                return None;
            };
            collect_assumed_expr(function, argument, syntax)?;
            syntax.recursive_calls.insert(expression.id);
            Some(())
        }
        ExprKind::Constant(Constant::Float(_) | Constant::Text(_))
        | ExprKind::Tuple(_)
        | ExprKind::List(_)
        | ExprKind::Match { .. }
        | ExprKind::Record { .. }
        | ExprKind::Variant { .. }
        | ExprKind::Refine { .. }
        | ExprKind::Unrefine(_)
        | ExprKind::Call { .. }
        | ExprKind::MakeView { .. }
        | ExprKind::ReborrowView { .. }
        | ExprKind::Await { .. }
        | ExprKind::Sleep { .. }
        | ExprKind::WaitFd { .. }
        | ExprKind::TaskJoin { .. }
        | ExprKind::Copy(_)
        | ExprKind::Move(_) => None,
    }
}

const fn is_exact_scalar_type(ty: &Type) -> bool {
    matches!(ty, Type::Unit | Type::Bool | Type::Int)
}

struct ExactEvaluator<'function, 'results, 'proof> {
    function: &'function Function,
    input: i64,
    exact_results: &'results [i64],
    proof: &'proof mut ExactProof,
}

impl ExactEvaluator<'_, '_, '_> {
    fn evaluate(&mut self) -> Option<ExactValue> {
        let parameter = self.function.params.first()?;
        let mut environment = BTreeMap::from([(parameter.id, ExactValue::Int(self.input))]);
        self.eval_block(&self.function.body, &mut environment)
    }

    fn eval_block(
        &mut self,
        block: &Block,
        environment: &mut BTreeMap<LocalId, ExactValue>,
    ) -> Option<ExactValue> {
        for statement in &block.statements {
            let StatementKind::Let { local, value } = &statement.kind else {
                return None;
            };
            let value = self.eval_expr(value, environment)?;
            environment.insert(*local, value);
        }
        self.eval_expr(block.tail.as_deref()?, environment)
    }

    fn eval_expr(
        &mut self,
        expression: &Expr,
        environment: &mut BTreeMap<LocalId, ExactValue>,
    ) -> Option<ExactValue> {
        match &expression.kind {
            ExprKind::Constant(Constant::Unit) => Some(ExactValue::Unit),
            ExprKind::Constant(Constant::Bool(value)) => Some(ExactValue::Bool(*value)),
            ExprKind::Constant(Constant::Int(value)) => Some(ExactValue::Int(*value)),
            ExprKind::Copy(place) if place.projection.is_empty() => {
                environment.get(&place.local).copied()
            }
            ExprKind::Move(place) if place.projection.is_empty() => {
                environment.remove(&place.local)
            }
            ExprKind::Unary(operator, value) => {
                let value = self.eval_expr(value, environment)?;
                let result = match (operator, value) {
                    (UnaryOp::Not, ExactValue::Bool(value)) => ExactValue::Bool(!value),
                    (UnaryOp::Negate, ExactValue::Int(value)) => {
                        ExactValue::Int(value.checked_neg()?)
                    }
                    _ => return None,
                };
                if *operator == UnaryOp::Negate {
                    self.proof.arithmetic.insert(expression.id);
                }
                Some(result)
            }
            ExprKind::Binary(operator, left, right) => {
                self.eval_binary(expression.id, *operator, left, right, environment)
            }
            ExprKind::Block(block) => self.eval_block(block, environment),
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let ExactValue::Bool(condition) = self.eval_expr(condition, environment)? else {
                    return None;
                };
                if condition {
                    self.eval_block(then_branch, environment)
                } else {
                    self.eval_block(else_branch, environment)
                }
            }
            ExprKind::Call {
                target: CallTarget::Direct(callee),
                type_arguments,
                arguments,
                witnesses,
            } if *callee == self.function.id
                && type_arguments.is_empty()
                && witnesses.is_empty() =>
            {
                let [CallArgument::Value(argument)] = arguments.as_slice() else {
                    return None;
                };
                let ExactValue::Int(argument) = self.eval_expr(argument, environment)? else {
                    return None;
                };
                if argument < 0 || argument >= self.input {
                    return None;
                }
                let result = *self.exact_results.get(usize::try_from(argument).ok()?)?;
                self.proof.recursive_calls.insert(expression.id);
                Some(ExactValue::Int(result))
            }
            _ => None,
        }
    }

    fn eval_binary(
        &mut self,
        expression: ExprId,
        operator: BinaryOp,
        left: &Expr,
        right: &Expr,
        environment: &mut BTreeMap<LocalId, ExactValue>,
    ) -> Option<ExactValue> {
        if operator == BinaryOp::And || operator == BinaryOp::Or {
            return self.eval_logical(operator, left, right, environment);
        }
        let left = self.eval_expr(left, environment)?;
        let right = self.eval_expr(right, environment)?;
        let result = match (operator, left, right) {
            (BinaryOp::Add, ExactValue::Int(left), ExactValue::Int(right)) => {
                ExactValue::Int(exact_checked_integer(BinaryOp::Add, left, right)?)
            }
            (BinaryOp::Subtract, ExactValue::Int(left), ExactValue::Int(right)) => {
                ExactValue::Int(exact_checked_integer(BinaryOp::Subtract, left, right)?)
            }
            (BinaryOp::Multiply, ExactValue::Int(left), ExactValue::Int(right)) => {
                ExactValue::Int(exact_checked_integer(BinaryOp::Multiply, left, right)?)
            }
            (BinaryOp::Divide, ExactValue::Int(left), ExactValue::Int(right)) => {
                ExactValue::Int(exact_checked_integer(BinaryOp::Divide, left, right)?)
            }
            (BinaryOp::Equal, left, right) => ExactValue::Bool(left == right),
            (BinaryOp::NotEqual, left, right) => ExactValue::Bool(left != right),
            (BinaryOp::Less, ExactValue::Int(left), ExactValue::Int(right)) => {
                ExactValue::Bool(left < right)
            }
            (BinaryOp::LessEqual, ExactValue::Int(left), ExactValue::Int(right)) => {
                ExactValue::Bool(left <= right)
            }
            (BinaryOp::Greater, ExactValue::Int(left), ExactValue::Int(right)) => {
                ExactValue::Bool(left > right)
            }
            (BinaryOp::GreaterEqual, ExactValue::Int(left), ExactValue::Int(right)) => {
                ExactValue::Bool(left >= right)
            }
            _ => return None,
        };
        if matches!(
            operator,
            BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide
        ) {
            self.proof.arithmetic.insert(expression);
        }
        Some(result)
    }

    fn eval_logical(
        &mut self,
        operator: BinaryOp,
        left: &Expr,
        right: &Expr,
        environment: &mut BTreeMap<LocalId, ExactValue>,
    ) -> Option<ExactValue> {
        let ExactValue::Bool(left) = self.eval_expr(left, environment)? else {
            return None;
        };
        if (operator == BinaryOp::And && !left) || (operator == BinaryOp::Or && left) {
            return Some(ExactValue::Bool(left));
        }
        let ExactValue::Bool(right) = self.eval_expr(right, environment)? else {
            return None;
        };
        Some(ExactValue::Bool(right))
    }
}

fn exact_checked_integer(operator: BinaryOp, left: i64, right: i64) -> Option<i64> {
    let result = match operator {
        BinaryOp::Add => i128::from(left).checked_add(i128::from(right))?,
        BinaryOp::Subtract => i128::from(left).checked_sub(i128::from(right))?,
        BinaryOp::Multiply => i128::from(left).checked_mul(i128::from(right))?,
        BinaryOp::Divide => {
            if right == 0 || (left == i64::MIN && right == -1) {
                return None;
            }
            i128::from(left) / i128::from(right)
        }
        _ => return None,
    };
    i64::try_from(result).ok()
}

fn set_unknown_entry(program: &Program, function: &Function, context: &mut FunctionContext) {
    context.reached = true;
    for (slot, parameter) in context.parameters.iter_mut().zip(&function.params) {
        *slot = Some(unknown_value(program, &parameter.ty, 0));
    }
}

struct FunctionAnalyzer<'program, 'work> {
    program: &'program Program,
    function_id: FunctionId,
    observations: &'work mut Vec<CallObservation>,
    proof_visits: &'work mut BTreeMap<ExprSite, bool>,
    record_proofs: bool,
    call_stack: BTreeSet<FunctionId>,
    conservative_calls: bool,
}

impl FunctionAnalyzer<'_, '_> {
    fn scan(&mut self, function: &Function, context: &FunctionContext) {
        let mut environment = RangeEnv::new();
        for (parameter, value) in function.params.iter().zip(&context.parameters) {
            environment.insert(
                parameter.id,
                value
                    .clone()
                    .unwrap_or_else(|| unknown_value(self.program, &parameter.ty, 0)),
            );
        }
        self.eval_block(&function.body, &mut environment);
    }

    fn eval_block(&mut self, block: &Block, environment: &mut RangeEnv) -> AbstractValue {
        for statement in &block.statements {
            self.eval_statement(&statement.kind, environment);
        }
        block
            .tail
            .as_deref()
            .map_or(AbstractValue::Top, |tail| self.eval_expr(tail, environment))
    }

    #[allow(clippy::too_many_lines)]
    fn eval_statement(&mut self, statement: &StatementKind, environment: &mut RangeEnv) {
        match statement {
            StatementKind::Let { local, value } => {
                let mut result = self.eval_expr(value, environment);
                // A list copied into a new binding can share backing storage. Lose both aliases
                // until MIR carries an explicit uniqueness fact.
                if matches!(result, AbstractValue::IntList(_))
                    && matches!(value.kind, ExprKind::Copy(_) | ExprKind::Move(_))
                {
                    if let ExprKind::Copy(place) | ExprKind::Move(place) = &value.kind {
                        environment.insert(place.local, AbstractValue::Top);
                    }
                    result = AbstractValue::Top;
                }
                environment.insert(*local, result);
            }
            StatementKind::LetTuple { locals, value } => {
                self.eval_expr(value, environment);
                for local in locals {
                    environment.insert(*local, AbstractValue::Top);
                }
            }
            StatementKind::Assign { place, value } => {
                let result = self.eval_expr(value, environment);
                write_place(environment, place, result);
            }
            StatementKind::ForRange {
                local,
                start,
                end,
                body,
            } => self.eval_counted_loop(*local, start, end, body, environment),
            StatementKind::Assert { condition } => {
                self.eval_expr(condition, environment);
                refine_condition(condition, true, environment);
            }
            StatementKind::Evaluate(value) => {
                self.eval_expr(value, environment);
            }
            StatementKind::Defer(cleanup) => {
                // Proof recording is disabled for this function. Context propagation must allow
                // cleanup to observe either entry or exit state.
                let mut cleanup_environment = environment.clone();
                self.eval_block(cleanup, &mut cleanup_environment);
                *environment = join_environments(environment, &cleanup_environment);
            }
            StatementKind::Return(value) => {
                if let Some(value) = value {
                    self.eval_expr(value, environment);
                }
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn eval_counted_loop(
        &mut self,
        local: LocalId,
        start: &Expr,
        end: &Expr,
        body: &Block,
        environment: &mut RangeEnv,
    ) {
        let start = self.eval_expr(start, environment).int_range();
        let end = self.eval_expr(end, environment).int_range();
        let iteration = end
            .upper
            .checked_sub(1)
            .and_then(|upper| (start.lower <= upper).then_some(IntRange::new(start.lower, upper)));
        let count = counted_iteration_upper(start, end);
        let base = environment.clone();
        let mutated = mutated_roots(body);

        if count == Some(0) {
            environment.insert(local, AbstractValue::Int(IntFact::range(IntRange::FULL)));
            return;
        }

        // Checked MIR requires an immutable induction binding. Keep the analysis fail-closed for
        // unchecked callers too: codegen advances the physical loop slot after the body, so a
        // body assignment would invalidate the static iteration interval used by every proof.
        if mutated.contains(&local) {
            let mut body_environment = base.clone();
            for root in &mutated {
                body_environment.insert(*root, AbstractValue::Top);
            }
            body_environment.insert(local, AbstractValue::Int(IntFact::range(IntRange::FULL)));
            self.eval_block(body, &mut body_environment);
            for root in mutated {
                environment.insert(root, AbstractValue::Top);
            }
            environment.insert(local, AbstractValue::Int(IntFact::range(IntRange::FULL)));
            return;
        }

        let Some(count) = count else {
            let mut body_environment = base.clone();
            for root in &mutated {
                body_environment.insert(*root, AbstractValue::Top);
            }
            body_environment.insert(
                local,
                AbstractValue::Int(IntFact::range(iteration.unwrap_or(IntRange::FULL))),
            );
            self.eval_block(body, &mut body_environment);
            for root in mutated {
                environment.insert(root, AbstractValue::Top);
            }
            environment.insert(local, AbstractValue::Int(IntFact::range(IntRange::FULL)));
            return;
        };

        // A bounded non-linear transformer (for example a canonical remainder) can converge even
        // when it is not an additive recurrence. Keep this fixed compiler budget as a secondary
        // invariant path; additive aggregates use the closed form below instead of unrolling.
        let mut invariant = base.clone();
        invariant.insert(
            local,
            AbstractValue::Int(IntFact::range(iteration.unwrap_or(IntRange::FULL))),
        );
        let mut invariant_converged = false;
        for _ in 0..LOOP_FIXPOINT_LIMIT {
            let mut body_environment = invariant.clone();
            let was_recording = self.record_proofs;
            self.record_proofs = false;
            self.eval_block(body, &mut body_environment);
            self.record_proofs = was_recording;
            let mut next = base.clone();
            for root in &mutated {
                let base_value = base.get(root).cloned().unwrap_or(AbstractValue::Top);
                let body_value = body_environment
                    .get(root)
                    .cloned()
                    .unwrap_or(AbstractValue::Top);
                next.insert(*root, base_value.join(&body_value).without_affine());
            }
            next.insert(
                local,
                AbstractValue::Int(IntFact::range(iteration.unwrap_or(IntRange::FULL))),
            );
            if next == invariant {
                invariant_converged = true;
                break;
            }
            invariant = next;
        }

        // Seed only syntactically mutated roots. Loop-invariant inputs remain interval constants,
        // so `x = x + invariant` is a translation while `x = x + y` is rejected when y mutates.
        let mut symbolic = base.clone();
        for root in &mutated {
            if let Some(value) = symbolic.get_mut(root) {
                seed_origins(
                    value,
                    self.function_id,
                    PlaceSite {
                        local: *root,
                        projection: Vec::new(),
                    },
                );
            }
        }
        symbolic.insert(
            local,
            AbstractValue::Int(IntFact::range(iteration.unwrap_or(IntRange::FULL))),
        );
        let was_recording = self.record_proofs;
        self.record_proofs = false;
        self.eval_block(body, &mut symbolic);
        self.record_proofs = was_recording;

        let mut pre_iteration = base.clone();
        let mut after_loop = base.clone();
        for root in &mutated {
            let base_value = base.get(root).cloned().unwrap_or(AbstractValue::Top);
            let symbolic_value = symbolic.get(root).cloned().unwrap_or(AbstractValue::Top);
            let place = PlaceSite {
                local: *root,
                projection: Vec::new(),
            };
            let (mut pre, mut after) = close_recurrence(
                &base_value,
                &symbolic_value,
                self.function_id,
                &place,
                count,
            );
            if invariant_converged
                && matches!(pre, AbstractValue::Top)
                && let Some(fixed) = invariant.get(root)
            {
                pre = fixed.clone().without_affine();
                after = pre.clone();
            }
            pre_iteration.insert(*root, pre);
            after_loop.insert(*root, after);
        }

        // Close dependencies which are not represented by an affine recurrence. In particular,
        // a List element envelope can depend on another mutated scalar whose closed range is much
        // wider than its first symbolic value. Recording proofs before this inductive join would
        // under-approximate elements appended in later iterations.
        let mut proof_environment = pre_iteration;
        proof_environment.insert(
            local,
            AbstractValue::Int(IntFact::range(iteration.unwrap_or(IntRange::FULL))),
        );
        let mut proof_envelope_converged = false;
        for _ in 0..LOOP_FIXPOINT_LIMIT {
            let mut transferred = proof_environment.clone();
            let was_recording = self.record_proofs;
            self.record_proofs = false;
            self.eval_block(body, &mut transferred);
            self.record_proofs = was_recording;
            let mut next = proof_environment.clone();
            for root in &mutated {
                let before = proof_environment
                    .get(root)
                    .cloned()
                    .unwrap_or(AbstractValue::Top);
                let after = transferred.get(root).cloned().unwrap_or(AbstractValue::Top);
                next.insert(*root, join_non_recurrent_envelopes(&before, &after));
            }
            next.insert(
                local,
                AbstractValue::Int(IntFact::range(iteration.unwrap_or(IntRange::FULL))),
            );
            if next == proof_environment {
                proof_envelope_converged = true;
                break;
            }
            proof_environment = next;
        }
        if proof_envelope_converged {
            for root in &mutated {
                if let Some(value) = proof_environment.get_mut(root) {
                    stabilize_non_recurrent_envelopes(value);
                }
            }
        } else {
            for root in &mutated {
                if let Some(value) = proof_environment.get_mut(root) {
                    widen_non_recurrent_envelopes(value);
                }
            }
        }

        // The symbolic recurrence and the aggregate envelope are complementary abstractions.
        // Validate their composition before trusting either one: a one-step transfer from any
        // represented pre-state must remain inside the union of all pre-states and the final
        // state. If cross-root state evolution escapes that closed envelope, discard every
        // mutated fact for this loop. Keeping only one root could retain a proof that depended on
        // another root whose recurrence failed.
        let mut transferred = proof_environment.clone();
        let was_recording = self.record_proofs;
        self.record_proofs = false;
        self.eval_block(body, &mut transferred);
        self.record_proofs = was_recording;
        let transfer_is_closed = mutated.iter().all(|root| {
            let before = proof_environment
                .get(root)
                .cloned()
                .unwrap_or(AbstractValue::Top);
            let final_state = after_loop.get(root).cloned().unwrap_or(AbstractValue::Top);
            let allowed = before.join(&final_state).without_affine();
            transferred
                .get(root)
                .is_some_and(|value| abstract_subset(value, &allowed))
        });
        if !transfer_is_closed {
            for root in &mutated {
                proof_environment.insert(*root, AbstractValue::Top);
                after_loop.insert(*root, AbstractValue::Top);
            }
        }

        // Record only after the pre-iteration environment is an inductive envelope. A check
        // disappears only if it is safe for every state represented here.
        self.eval_block(body, &mut proof_environment);

        // Non-recurrent aggregate components can still depend on a recurrent scalar. For example,
        // `list.add(counter)` needs the closed counter envelope, not the first iteration's element.
        // Joining the closed-state transfer prevents a single-step element envelope from escaping.
        for root in &mutated {
            let Some(after) = after_loop.get_mut(root) else {
                continue;
            };
            let post = proof_environment
                .get(root)
                .cloned()
                .unwrap_or(AbstractValue::Top);
            *after = after.join(&post).without_affine();
            stabilize_non_recurrent_envelopes(after);
        }

        *environment = after_loop;
        environment.insert(local, AbstractValue::Int(IntFact::range(IntRange::FULL)));
    }

    #[allow(clippy::too_many_lines)]
    fn eval_expr(&mut self, expression: &Expr, environment: &mut RangeEnv) -> AbstractValue {
        match &expression.kind {
            ExprKind::Constant(Constant::Int(value)) => AbstractValue::Int(IntFact::exact(*value)),
            ExprKind::Constant(_) => AbstractValue::Top,
            ExprKind::Copy(place) | ExprKind::Move(place) => read_place(environment, place),
            ExprKind::Unary(operator, value) => {
                let value = self.eval_expr(value, environment);
                if expression.ty != Type::Int || *operator != UnaryOp::Negate {
                    return AbstractValue::Top;
                }
                let range = value.int_range();
                let safe = range.lower != i64::MIN;
                self.record(expression, safe);
                if !safe {
                    return AbstractValue::Int(IntFact {
                        range: IntRange::FULL,
                        affine: None,
                    });
                }
                let affine = match value {
                    AbstractValue::Int(value) => value.affine.and_then(AffineForm::negate),
                    _ => None,
                };
                AbstractValue::Int(IntFact {
                    range: IntRange::new(-range.upper, -range.lower),
                    affine,
                })
            }
            ExprKind::Binary(operator, left, right) => {
                let left_value = self.eval_expr(left, environment);
                if matches!(operator, BinaryOp::And | BinaryOp::Or) {
                    // The right side is conditional. Join the skipped and executed
                    // states so an InOut call or assignment on the RHS is never
                    // mistaken for an unconditional post-state.
                    let skipped = environment.clone();
                    let mut executed = environment.clone();
                    self.eval_expr(right, &mut executed);
                    *environment = join_environments(&skipped, &executed);
                    return AbstractValue::Top;
                }
                let right_value = self.eval_expr(right, environment);
                if expression.ty != Type::Int
                    || !matches!(
                        operator,
                        BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide
                    )
                {
                    return AbstractValue::Top;
                }
                let (mut result, safe) = checked_binary_fact(*operator, &left_value, &right_value);
                if *operator == BinaryOp::Subtract
                    && safe
                    && let Some(modulus) = remainder_modulus(left, right, environment)
                {
                    let independent_nonnegative = matches!(
                        &left_value,
                        AbstractValue::Int(IntFact {
                            range,
                            affine: Some(affine),
                        }) if affine.terms.is_empty() && range.lower >= 0
                    );
                    result.range = if independent_nonnegative {
                        IntRange::new(0, modulus - 1)
                    } else {
                        IntRange::new(-(modulus - 1), modulus - 1)
                    };
                    result.affine =
                        independent_nonnegative.then(|| AffineForm::range(result.range));
                }
                self.record(expression, safe);
                AbstractValue::Int(result)
            }
            ExprKind::Block(block) => self.eval_block(block, environment),
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.eval_expr(condition, environment);
                let mut then_environment = environment.clone();
                refine_condition(condition, true, &mut then_environment);
                let then_value = self.eval_block(then_branch, &mut then_environment);
                let mut else_environment = environment.clone();
                refine_condition(condition, false, &mut else_environment);
                let else_value = self.eval_block(else_branch, &mut else_environment);
                *environment = join_environments(&then_environment, &else_environment);
                then_value.join(&else_value)
            }
            ExprKind::Match { scrutinee, arms } => {
                let scrutinee = self.eval_expr(scrutinee, environment);
                let mut joined_environment = None;
                let mut joined_value = None;
                for arm in arms {
                    let Some(bindings) = pattern_bindings(&arm.pattern, &scrutinee) else {
                        continue;
                    };
                    let mut arm_environment = environment.clone();
                    for (binding, value) in arm.bindings.iter().zip(bindings) {
                        arm_environment.insert(*binding, value);
                    }
                    let value = self.eval_expr(&arm.value, &mut arm_environment);
                    joined_environment = Some(joined_environment.map_or_else(
                        || arm_environment.clone(),
                        |current| join_environments(&current, &arm_environment),
                    ));
                    joined_value = Some(joined_value.map_or_else(
                        || value.clone(),
                        |current: AbstractValue| current.join(&value),
                    ));
                }
                if let Some(joined) = joined_environment {
                    *environment = joined;
                }
                joined_value.unwrap_or(AbstractValue::Top)
            }
            ExprKind::Call {
                target,
                arguments,
                witnesses,
                ..
            } => self.eval_call(
                target,
                arguments,
                !witnesses.is_empty(),
                &expression.ty,
                environment,
                Some(expression.id),
            ),
            ExprKind::Tuple(values) => {
                for value in values {
                    self.eval_expr(value, environment);
                }
                AbstractValue::Top
            }
            ExprKind::List(values) => {
                let mut envelope = None;
                let mut element_stable = true;
                let is_int_list =
                    matches!(&expression.ty, Type::List(element) if **element == Type::Int);
                for value in values {
                    let value = self.eval_expr(value, environment);
                    if is_int_list {
                        let range = value.int_range();
                        element_stable &= matches!(
                            &value,
                            AbstractValue::Int(IntFact {
                                affine: Some(affine),
                                ..
                            }) if affine.terms.is_empty()
                        );
                        envelope =
                            Some(envelope.map_or(range, |current: IntRange| current.join(range)));
                    }
                }
                if is_int_list {
                    let Ok(length) = i64::try_from(values.len()) else {
                        return AbstractValue::Top;
                    };
                    AbstractValue::IntList(ListFacts {
                        element: envelope,
                        element_stable,
                        length: IntFact::exact(length),
                    })
                } else {
                    AbstractValue::Top
                }
            }
            ExprKind::Record { ty, fields, .. } => AbstractValue::Record {
                ty: *ty,
                fields: fields
                    .iter()
                    .map(|field| self.eval_expr(field, environment))
                    .collect(),
            },
            ExprKind::Variant {
                ty,
                variant,
                payload,
                ..
            } => AbstractValue::Enum(EnumFacts {
                ty: *ty,
                variants: BTreeMap::from([(
                    *variant,
                    payload
                        .iter()
                        .map(|value| self.eval_expr(value, environment))
                        .collect(),
                )]),
            }),
            ExprKind::Refine { value, .. } | ExprKind::Unrefine(value) => {
                self.eval_expr(value, environment)
            }
            ExprKind::MakeView {
                value, writeback, ..
            } => {
                self.eval_expr(value, environment);
                if let Some(place) = writeback {
                    poison_root(environment, place.local);
                }
                AbstractValue::Top
            }
            ExprKind::ReborrowView { owner, .. } => {
                poison_root(environment, owner.local);
                AbstractValue::Top
            }
            ExprKind::Await { task, .. } => {
                self.eval_expr(task, environment);
                poison_aggregates(environment);
                AbstractValue::Top
            }
            ExprKind::Sleep { milliseconds } => {
                self.eval_expr(milliseconds, environment);
                AbstractValue::Top
            }
            ExprKind::WaitFd { descriptor, .. } => {
                self.eval_expr(descriptor, environment);
                AbstractValue::Top
            }
            ExprKind::TaskJoin { arguments, .. } => {
                for argument in arguments {
                    self.eval_expr(argument, environment);
                }
                poison_aggregates(environment);
                AbstractValue::Top
            }
        }
    }

    fn eval_call(
        &mut self,
        target: &CallTarget,
        arguments: &[CallArgument],
        has_witnesses: bool,
        result_ty: &Type,
        environment: &mut RangeEnv,
        expression: Option<ExprId>,
    ) -> AbstractValue {
        let inout_alias_hazard = call_has_later_move_alias(arguments);
        let mut values = Vec::with_capacity(arguments.len());
        let mut inout = Vec::with_capacity(arguments.len());
        for argument in arguments {
            match argument {
                CallArgument::Value(value) => {
                    values.push(Some(self.eval_expr(value, environment)));
                    inout.push(None);
                }
                CallArgument::InOut(place) => {
                    // An InOut argument denotes an alias, not an eager value read.
                    // Later value arguments execute before the callee observes it
                    // and may mutate the same root.
                    values.push(None);
                    inout.push(Some(place.clone()));
                }
            }
        }
        let values = values
            .into_iter()
            .zip(&inout)
            .map(|(value, place)| {
                value.unwrap_or_else(|| {
                    let place = place
                        .as_ref()
                        .expect("an absent call value has an InOut place");
                    if place.projection.is_empty() {
                        read_place(environment, place)
                    } else {
                        // The current native ABI captures a projected ValueNode pointer before
                        // later value arguments execute. A later argument can replace the root,
                        // so reading the new projection here would describe a different object
                        // from the one observed by the callee. Keep projected InOut unknown until
                        // codegen establishes call proxies after argument evaluation.
                        AbstractValue::Top
                    }
                })
            })
            .collect::<Vec<_>>();

        if inout_alias_hazard {
            self.observe_unknown_target(target, has_witnesses);
            for place in inout.into_iter().flatten() {
                poison_root(environment, place.local);
            }
            return unknown_value(self.program, result_ty, 0);
        }

        if let CallTarget::Builtin(builtin) = target {
            return self.eval_builtin(*builtin, &values, &inout, result_ty, environment);
        }

        let function = match target {
            CallTarget::Direct(function) | CallTarget::Inherent(function) if !has_witnesses => {
                *function
            }
            CallTarget::Direct(_)
            | CallTarget::Inherent(_)
            | CallTarget::StaticConcept { .. }
            | CallTarget::Dynamic { .. } => {
                for place in inout.into_iter().flatten() {
                    poison_root(environment, place.local);
                }
                return unknown_value(self.program, result_ty, 0);
            }
            CallTarget::Builtin(_) => unreachable!("handled above"),
        };

        if self.conservative_calls {
            if let Some(callee) = self.program.function(function) {
                self.observations.push(CallObservation {
                    function,
                    arguments: callee
                        .params
                        .iter()
                        .map(|parameter| unknown_value(self.program, &parameter.ty, 0))
                        .collect(),
                    site: None,
                });
            }
            for place in inout.into_iter().flatten() {
                poison_root(environment, place.local);
            }
            return unknown_value(self.program, result_ty, 0);
        }

        self.observations.push(CallObservation {
            function,
            arguments: values.iter().map(AbstractValue::without_affine).collect(),
            site: expression.map(|expression| ExprSite {
                function: self.function_id,
                expression,
            }),
        });
        let Some(summary) = self.summarize_transparent(function, &values) else {
            for place in inout.into_iter().flatten() {
                poison_root(environment, place.local);
            }
            return unknown_value(self.program, result_ty, 0);
        };
        for ((place, value), argument) in inout.into_iter().zip(summary.parameters).zip(arguments) {
            if matches!(argument, CallArgument::InOut(_))
                && let Some(place) = place
            {
                if place.projection.is_empty() {
                    write_place(environment, &place, value);
                } else {
                    // The callee may have updated a detached pre-argument node. No post-state of
                    // the current root projection is justified by this ABI.
                    poison_root(environment, place.local);
                }
            }
        }
        summary.result
    }

    fn observe_unknown_target(&mut self, target: &CallTarget, has_witnesses: bool) {
        let (CallTarget::Direct(function) | CallTarget::Inherent(function)) = target else {
            return;
        };
        if has_witnesses {
            return;
        }
        let Some(callee) = self.program.function(*function) else {
            return;
        };
        self.observations.push(CallObservation {
            function: *function,
            arguments: callee
                .params
                .iter()
                .map(|parameter| unknown_value(self.program, &parameter.ty, 0))
                .collect(),
            site: None,
        });
    }

    fn eval_builtin(
        &mut self,
        builtin: Builtin,
        values: &[AbstractValue],
        inout: &[Option<Place>],
        result_ty: &Type,
        environment: &mut RangeEnv,
    ) -> AbstractValue {
        match builtin {
            Builtin::ListAdd => {
                let Some(Some(place)) = inout.first() else {
                    return AbstractValue::Top;
                };
                let Some(element) = values.get(1) else {
                    poison_root(environment, place.local);
                    return AbstractValue::Top;
                };
                let AbstractValue::IntList(mut list) = values[0].clone() else {
                    poison_root(environment, place.local);
                    return AbstractValue::Top;
                };
                let element_stable = matches!(
                    element,
                    AbstractValue::Int(IntFact {
                        affine: Some(affine),
                        ..
                    }) if affine.terms.is_empty()
                );
                let element = element.int_range();
                list.element = Some(
                    list.element
                        .map_or(element, |current| current.join(element)),
                );
                list.element_stable &= element_stable;
                list.length = checked_binary_fact(
                    BinaryOp::Add,
                    &AbstractValue::Int(list.length),
                    &AbstractValue::Int(IntFact::exact(1)),
                )
                .0;
                write_place(environment, place, AbstractValue::IntList(list));
                AbstractValue::Top
            }
            Builtin::ListLength => values
                .first()
                .map_or(AbstractValue::Top, |value| match value {
                    AbstractValue::IntList(list) => AbstractValue::Int(list.length.clone()),
                    _ => AbstractValue::Int(IntFact::range(IntRange::FULL)),
                }),
            Builtin::ListGet => {
                let Some(option) = self.program.prelude.option else {
                    return AbstractValue::Top;
                };
                let Some(AbstractValue::IntList(list)) = values.first() else {
                    // An unknown receiver may be non-empty. Returning a None-only
                    // enum here would incorrectly make the Some arm unreachable.
                    return AbstractValue::Top;
                };
                let mut variants = BTreeMap::from([(VariantId(0), Vec::new())]);
                if let Some(element) = list.element {
                    let element = if list.element_stable {
                        IntFact::range(element)
                    } else {
                        IntFact::dependent_range(element)
                    };
                    variants.insert(VariantId(1), vec![AbstractValue::Int(element)]);
                }
                // The Some payload may carry a range, but absence remains a runtime outcome.
                AbstractValue::Enum(EnumFacts {
                    ty: option,
                    variants,
                })
            }
            _ => {
                for place in inout.iter().flatten() {
                    poison_root(environment, place.local);
                }
                unknown_value(self.program, result_ty, 0)
            }
        }
    }

    fn summarize_transparent(
        &mut self,
        function_id: FunctionId,
        arguments: &[AbstractValue],
    ) -> Option<CallSummary> {
        if self.call_stack.contains(&function_id) {
            return None;
        }
        let function = self.program.function(function_id)?;
        if function.is_async
            || !function.witness_params.is_empty()
            || block_contains_defer(&function.body)
            || block_contains_loop(&function.body)
            || block_contains_return(&function.body)
        {
            return None;
        }

        let mut environment = RangeEnv::new();
        for (index, parameter) in function.params.iter().enumerate() {
            environment.insert(
                parameter.id,
                arguments
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| unknown_value(self.program, &parameter.ty, 0)),
            );
        }
        let mut ignored_observations = Vec::new();
        let mut ignored_proofs = BTreeMap::new();
        let mut call_stack = self.call_stack.clone();
        call_stack.insert(function_id);
        let mut nested = FunctionAnalyzer {
            program: self.program,
            function_id,
            observations: &mut ignored_observations,
            proof_visits: &mut ignored_proofs,
            record_proofs: false,
            call_stack,
            conservative_calls: false,
        };
        let result = nested.eval_block(&function.body, &mut environment);
        Some(CallSummary {
            result,
            parameters: function
                .params
                .iter()
                .map(|parameter| {
                    environment
                        .get(&parameter.id)
                        .cloned()
                        .unwrap_or_else(|| unknown_value(self.program, &parameter.ty, 0))
                })
                .collect(),
        })
    }

    fn record(&mut self, expression: &Expr, safe: bool) {
        if !self.record_proofs {
            return;
        }
        self.proof_visits
            .entry(ExprSite {
                function: self.function_id,
                expression: expression.id,
            })
            .and_modify(|known| *known &= safe)
            .or_insert(safe);
    }
}

fn normalize_for_type(program: &Program, ty: &Type, value: AbstractValue) -> AbstractValue {
    match (ty, value) {
        (Type::Int, AbstractValue::Int(value)) => AbstractValue::Int(value.without_affine()),
        (Type::Nominal(expected, _), value @ AbstractValue::Record { ty, .. })
            if expected == &ty =>
        {
            value.without_affine()
        }
        (Type::List(element), value @ AbstractValue::IntList(_)) if **element == Type::Int => {
            value.without_affine()
        }
        (Type::Nominal(expected, _), value @ AbstractValue::Enum(EnumFacts { ty, .. }))
            if expected == &ty =>
        {
            value.without_affine()
        }
        (_, _) => unknown_value(program, ty, 0),
    }
}

fn unknown_value(program: &Program, ty: &Type, depth: usize) -> AbstractValue {
    if depth >= TYPE_EXPANSION_LIMIT {
        return AbstractValue::Top;
    }
    match ty {
        Type::Int => AbstractValue::Int(IntFact::range(IntRange::FULL)),
        Type::Nominal(ty, _) => match program.type_def(*ty).map(|definition| &definition.kind) {
            Some(TypeDefKind::Record { fields, .. }) => AbstractValue::Record {
                ty: *ty,
                fields: fields
                    .iter()
                    .map(|field| unknown_value(program, &field.ty, depth + 1))
                    .collect(),
            },
            _ => AbstractValue::Top,
        },
        _ => AbstractValue::Top,
    }
}

fn read_place(environment: &RangeEnv, place: &Place) -> AbstractValue {
    let Some(mut value) = environment.get(&place.local) else {
        return AbstractValue::Top;
    };
    for projection in &place.projection {
        let AbstractValue::Record { fields, .. } = value else {
            return AbstractValue::Top;
        };
        let Ok(index) = usize::try_from(*projection) else {
            return AbstractValue::Top;
        };
        let Some(field) = fields.get(index) else {
            return AbstractValue::Top;
        };
        value = field;
    }
    value.clone()
}

fn write_place(environment: &mut RangeEnv, place: &Place, value: AbstractValue) {
    if place.projection.is_empty() {
        environment.insert(place.local, value);
        return;
    }
    let Some(root) = environment.get_mut(&place.local) else {
        environment.insert(place.local, AbstractValue::Top);
        return;
    };
    if !write_projection(root, &place.projection, value) {
        *root = AbstractValue::Top;
    }
}

fn write_projection(target: &mut AbstractValue, projection: &[u32], value: AbstractValue) -> bool {
    let Some((first, rest)) = projection.split_first() else {
        *target = value;
        return true;
    };
    let AbstractValue::Record { fields, .. } = target else {
        return false;
    };
    let Ok(index) = usize::try_from(*first) else {
        return false;
    };
    let Some(field) = fields.get_mut(index) else {
        return false;
    };
    write_projection(field, rest, value)
}

fn poison_root(environment: &mut RangeEnv, local: LocalId) {
    environment.insert(local, AbstractValue::Top);
}

fn poison_aggregates(environment: &mut RangeEnv) {
    for value in environment.values_mut() {
        if !matches!(value, AbstractValue::Int(_)) {
            *value = AbstractValue::Top;
        }
    }
}

fn abstract_subset(value: &AbstractValue, envelope: &AbstractValue) -> bool {
    match (value, envelope) {
        (_, AbstractValue::Top) => true,
        (AbstractValue::Int(value), AbstractValue::Int(envelope)) => {
            range_subset(value.range, envelope.range)
        }
        (
            AbstractValue::Record {
                ty: value_ty,
                fields: value_fields,
            },
            AbstractValue::Record {
                ty: envelope_ty,
                fields: envelope_fields,
            },
        ) => {
            value_ty == envelope_ty
                && value_fields.len() == envelope_fields.len()
                && value_fields
                    .iter()
                    .zip(envelope_fields)
                    .all(|(value, envelope)| abstract_subset(value, envelope))
        }
        (AbstractValue::IntList(value), AbstractValue::IntList(envelope)) => {
            range_subset(value.length.range, envelope.length.range)
                && match (value.element, envelope.element) {
                    (None, _) => true,
                    (Some(_), None) => false,
                    (Some(value), Some(envelope)) => range_subset(value, envelope),
                }
        }
        (AbstractValue::Enum(value), AbstractValue::Enum(envelope)) => {
            value.ty == envelope.ty
                && value.variants.iter().all(|(variant, value_payload)| {
                    envelope
                        .variants
                        .get(variant)
                        .is_some_and(|envelope_payload| {
                            value_payload.len() == envelope_payload.len()
                                && value_payload
                                    .iter()
                                    .zip(envelope_payload)
                                    .all(|(value, envelope)| abstract_subset(value, envelope))
                        })
                })
        }
        _ => false,
    }
}

const fn range_subset(value: IntRange, envelope: IntRange) -> bool {
    value.lower >= envelope.lower && value.upper <= envelope.upper
}

/// Extends only facts which the affine recurrence closer does not model.
///
/// Scalar fields and list lengths already contain bounded closed-loop envelopes.
/// Repeatedly joining their one-step post-state would turn a finite counted
/// recurrence into an artificial unbounded loop and discard valid proofs.
fn join_non_recurrent_envelopes(
    current: &AbstractValue,
    transferred: &AbstractValue,
) -> AbstractValue {
    match (current, transferred) {
        (
            AbstractValue::Record {
                ty: current_ty,
                fields: current_fields,
            },
            AbstractValue::Record {
                ty: transferred_ty,
                fields: transferred_fields,
            },
        ) if current_ty == transferred_ty && current_fields.len() == transferred_fields.len() => {
            AbstractValue::Record {
                ty: *current_ty,
                fields: current_fields
                    .iter()
                    .zip(transferred_fields)
                    .map(|(current, transferred)| {
                        join_non_recurrent_envelopes(current, transferred)
                    })
                    .collect(),
            }
        }
        (AbstractValue::IntList(current), AbstractValue::IntList(transferred)) => {
            let element = match (current.element, transferred.element) {
                (Some(current), Some(transferred)) => Some(current.join(transferred)),
                (Some(value), None) | (None, Some(value)) => Some(value),
                (None, None) => None,
            };
            AbstractValue::IntList(ListFacts {
                element,
                element_stable: current.element_stable && transferred.element_stable,
                length: current.length.clone(),
            })
        }
        _ => current.clone(),
    }
}

fn widen_non_recurrent_envelopes(value: &mut AbstractValue) {
    match value {
        AbstractValue::Record { fields, .. } => {
            for field in fields {
                widen_non_recurrent_envelopes(field);
            }
        }
        AbstractValue::IntList(list) => {
            list.element = Some(IntRange::FULL);
            list.element_stable = true;
        }
        AbstractValue::Int(_) | AbstractValue::Enum(_) | AbstractValue::Top => {}
    }
}

fn stabilize_non_recurrent_envelopes(value: &mut AbstractValue) {
    match value {
        AbstractValue::Record { fields, .. } => {
            for field in fields {
                stabilize_non_recurrent_envelopes(field);
            }
        }
        AbstractValue::IntList(list) => list.element_stable = true,
        AbstractValue::Int(_) | AbstractValue::Enum(_) | AbstractValue::Top => {}
    }
}

fn join_environments(left: &RangeEnv, right: &RangeEnv) -> RangeEnv {
    let mut joined = BTreeMap::new();
    for local in left.keys().chain(right.keys()).copied() {
        let value = match (left.get(&local), right.get(&local)) {
            (Some(left), Some(right)) => left.join(right),
            (Some(_), None) | (None, Some(_)) => AbstractValue::Top,
            (None, None) => continue,
        };
        joined.insert(local, value);
    }
    joined
}

fn checked_binary_fact(
    operator: BinaryOp,
    left: &AbstractValue,
    right: &AbstractValue,
) -> (IntFact, bool) {
    let (range, safe) = checked_binary_range(operator, left.int_range(), right.int_range());
    if !safe {
        return (
            IntFact {
                range,
                affine: None,
            },
            false,
        );
    }
    let affine = match (left, right) {
        (AbstractValue::Int(left), AbstractValue::Int(right)) => match operator {
            BinaryOp::Add => left
                .affine
                .clone()
                .zip(right.affine.clone())
                .and_then(|(left, right)| left.combine(right, false)),
            BinaryOp::Subtract => left
                .affine
                .clone()
                .zip(right.affine.clone())
                .and_then(|(left, right)| left.combine(right, true)),
            BinaryOp::Multiply | BinaryOp::Divide => {
                let both_constant = left
                    .affine
                    .as_ref()
                    .is_some_and(|value| value.terms.is_empty())
                    && right
                        .affine
                        .as_ref()
                        .is_some_and(|value| value.terms.is_empty());
                both_constant.then(|| AffineForm::range(range))
            }
            _ => None,
        },
        _ => None,
    };
    (IntFact { range, affine }, true)
}

fn checked_binary_range(operator: BinaryOp, left: IntRange, right: IntRange) -> (IntRange, bool) {
    let bounds = match operator {
        BinaryOp::Add => vec![
            i128::from(left.lower) + i128::from(right.lower),
            i128::from(left.upper) + i128::from(right.upper),
        ],
        BinaryOp::Subtract => vec![
            i128::from(left.lower) - i128::from(right.upper),
            i128::from(left.upper) - i128::from(right.lower),
        ],
        BinaryOp::Multiply => vec![
            i128::from(left.lower) * i128::from(right.lower),
            i128::from(left.lower) * i128::from(right.upper),
            i128::from(left.upper) * i128::from(right.lower),
            i128::from(left.upper) * i128::from(right.upper),
        ],
        BinaryOp::Divide => {
            if (right.lower <= 0 && right.upper >= 0)
                || (left.lower == i64::MIN && right.lower <= -1 && right.upper >= -1)
            {
                return (IntRange::FULL, false);
            }
            let mut numerators = vec![left.lower, left.upper];
            if left.lower <= 0 && left.upper >= 0 {
                numerators.push(0);
            }
            let mut divisors = vec![right.lower, right.upper];
            if right.lower <= -1 && right.upper >= -1 {
                divisors.push(-1);
            }
            if right.lower <= 1 && right.upper >= 1 {
                divisors.push(1);
            }
            let mut values = Vec::new();
            for numerator in numerators {
                for divisor in &divisors {
                    if *divisor != 0 {
                        values.push(i128::from(numerator) / i128::from(*divisor));
                    }
                }
            }
            values
        }
        _ => return (IntRange::FULL, false),
    };
    let Some(lower) = bounds.iter().copied().min() else {
        return (IntRange::FULL, false);
    };
    let Some(upper) = bounds.iter().copied().max() else {
        return (IntRange::FULL, false);
    };
    checked_i128_range(lower, upper).map_or((IntRange::FULL, false), |range| (range, true))
}

fn checked_i128_range(lower: i128, upper: i128) -> Option<IntRange> {
    if lower < i128::from(i64::MIN) || upper > i128::from(i64::MAX) || lower > upper {
        None
    } else {
        Some(IntRange::new(
            i64::try_from(lower).ok()?,
            i64::try_from(upper).ok()?,
        ))
    }
}

fn counted_iteration_upper(start: IntRange, end: IntRange) -> Option<u64> {
    let count = (i128::from(end.upper) - i128::from(start.lower)).max(0);
    u64::try_from(count).ok()
}

fn seed_origins(value: &mut AbstractValue, function: FunctionId, place: PlaceSite) {
    match value {
        AbstractValue::Int(fact) => {
            *fact = IntFact::origin(
                fact.range,
                OriginSite {
                    function,
                    place,
                    kind: OriginKind::Integer,
                },
            );
        }
        AbstractValue::Record { fields, .. } => {
            for (index, field) in fields.iter_mut().enumerate() {
                let Ok(index) = u32::try_from(index) else {
                    *field = AbstractValue::Top;
                    continue;
                };
                let mut field_place = place.clone();
                field_place.projection.push(index);
                seed_origins(field, function, field_place);
            }
        }
        AbstractValue::IntList(list) => {
            list.length = IntFact::origin(
                list.length.range,
                OriginSite {
                    function,
                    place,
                    kind: OriginKind::ListLength,
                },
            );
            list.element_stable = false;
        }
        AbstractValue::Enum(_) => *value = AbstractValue::Top,
        AbstractValue::Top => {}
    }
}

fn close_recurrence(
    base: &AbstractValue,
    one_step: &AbstractValue,
    function: FunctionId,
    place: &PlaceSite,
    count: u64,
) -> (AbstractValue, AbstractValue) {
    match (base, one_step) {
        (AbstractValue::Int(base), AbstractValue::Int(one_step)) => {
            let origin = OriginSite {
                function,
                place: place.clone(),
                kind: OriginKind::Integer,
            };
            close_integer_recurrence(base, one_step, &origin, count)
                .map_or((AbstractValue::Top, AbstractValue::Top), |(pre, after)| {
                    (AbstractValue::Int(pre), AbstractValue::Int(after))
                })
        }
        (
            AbstractValue::Record {
                ty: base_ty,
                fields: base_fields,
            },
            AbstractValue::Record {
                ty: step_ty,
                fields: step_fields,
            },
        ) if base_ty == step_ty && base_fields.len() == step_fields.len() => {
            let mut pre_fields = Vec::with_capacity(base_fields.len());
            let mut after_fields = Vec::with_capacity(base_fields.len());
            for (index, (base, step)) in base_fields.iter().zip(step_fields).enumerate() {
                let Ok(index) = u32::try_from(index) else {
                    pre_fields.push(AbstractValue::Top);
                    after_fields.push(AbstractValue::Top);
                    continue;
                };
                let mut field_place = place.clone();
                field_place.projection.push(index);
                let (pre, after) = close_recurrence(base, step, function, &field_place, count);
                pre_fields.push(pre);
                after_fields.push(after);
            }
            (
                AbstractValue::Record {
                    ty: *base_ty,
                    fields: pre_fields,
                },
                AbstractValue::Record {
                    ty: *base_ty,
                    fields: after_fields,
                },
            )
        }
        (AbstractValue::IntList(base), AbstractValue::IntList(step)) => {
            let origin = OriginSite {
                function,
                place: place.clone(),
                kind: OriginKind::ListLength,
            };
            let Some((pre_length, after_length)) =
                close_integer_recurrence(&base.length, &step.length, &origin, count)
            else {
                return (AbstractValue::Top, AbstractValue::Top);
            };
            let element = match (base.element, step.element) {
                (Some(base), Some(step)) => Some(base.join(step)),
                (Some(value), None) | (None, Some(value)) => Some(value),
                (None, None) => None,
            };
            (
                AbstractValue::IntList(ListFacts {
                    element,
                    element_stable: base.element_stable && step.element_stable,
                    length: pre_length,
                }),
                AbstractValue::IntList(ListFacts {
                    element,
                    element_stable: base.element_stable && step.element_stable,
                    length: after_length,
                }),
            )
        }
        _ => (AbstractValue::Top, AbstractValue::Top),
    }
}

fn close_integer_recurrence(
    base: &IntFact,
    one_step: &IntFact,
    origin: &OriginSite,
    count: u64,
) -> Option<(IntFact, IntFact)> {
    let form = one_step.affine.as_ref()?;
    if form.terms.len() != 1 || form.terms.get(origin) != Some(&1) {
        return None;
    }
    let before_count = count.saturating_sub(1);
    let pre = repeated_translation_range(base.range, form.residual, before_count)?;
    let after = repeated_translation_range(base.range, form.residual, count)?;
    Some((IntFact::range(pre), IntFact::range(after)))
}

fn repeated_translation_range(
    base: IntRange,
    delta: IntRange,
    repetitions: u64,
) -> Option<IntRange> {
    let repetitions = i128::from(repetitions);
    let delta_values = [
        0,
        i128::from(delta.lower).checked_mul(repetitions)?,
        i128::from(delta.upper).checked_mul(repetitions)?,
    ];
    let delta_lower = *delta_values.iter().min()?;
    let delta_upper = *delta_values.iter().max()?;
    checked_i128_range(
        i128::from(base.lower).checked_add(delta_lower)?,
        i128::from(base.upper).checked_add(delta_upper)?,
    )
}

fn mutated_roots(block: &Block) -> BTreeSet<LocalId> {
    let mut roots = BTreeSet::new();
    collect_block_mutations(block, &mut roots);
    roots
}

fn collect_block_mutations(block: &Block, roots: &mut BTreeSet<LocalId>) {
    for statement in &block.statements {
        match &statement.kind {
            StatementKind::Assign { place, value } => {
                roots.insert(place.local);
                collect_expr_mutations(value, roots);
            }
            StatementKind::ForRange {
                start, end, body, ..
            } => {
                collect_expr_mutations(start, roots);
                collect_expr_mutations(end, roots);
                collect_block_mutations(body, roots);
            }
            StatementKind::Let { value, .. }
            | StatementKind::LetTuple { value, .. }
            | StatementKind::Evaluate(value) => collect_expr_mutations(value, roots),
            StatementKind::Assert { condition } => collect_expr_mutations(condition, roots),
            StatementKind::Defer(cleanup) => collect_block_mutations(cleanup, roots),
            StatementKind::Return(value) => {
                if let Some(value) = value {
                    collect_expr_mutations(value, roots);
                }
            }
        }
    }
    if let Some(tail) = &block.tail {
        collect_expr_mutations(tail, roots);
    }
}

fn collect_expr_mutations(expression: &Expr, roots: &mut BTreeSet<LocalId>) {
    match &expression.kind {
        ExprKind::Block(block) => collect_block_mutations(block, roots),
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            collect_expr_mutations(condition, roots);
            collect_block_mutations(then_branch, roots);
            collect_block_mutations(else_branch, roots);
        }
        ExprKind::Match { scrutinee, arms } => {
            collect_expr_mutations(scrutinee, roots);
            for arm in arms {
                collect_expr_mutations(&arm.value, roots);
            }
        }
        ExprKind::Call { arguments, .. } => {
            for argument in arguments {
                match argument {
                    CallArgument::Value(value) => collect_expr_mutations(value, roots),
                    CallArgument::InOut(place) => {
                        roots.insert(place.local);
                    }
                }
            }
        }
        ExprKind::MakeView {
            value, writeback, ..
        } => {
            collect_expr_mutations(value, roots);
            if let Some(place) = writeback {
                roots.insert(place.local);
            }
        }
        ExprKind::Tuple(values)
        | ExprKind::List(values)
        | ExprKind::Record { fields: values, .. }
        | ExprKind::Variant {
            payload: values, ..
        }
        | ExprKind::TaskJoin {
            arguments: values, ..
        } => {
            for value in values {
                collect_expr_mutations(value, roots);
            }
        }
        ExprKind::Unary(_, value)
        | ExprKind::Refine { value, .. }
        | ExprKind::Unrefine(value)
        | ExprKind::Await { task: value, .. }
        | ExprKind::Sleep {
            milliseconds: value,
        }
        | ExprKind::WaitFd {
            descriptor: value, ..
        } => collect_expr_mutations(value, roots),
        ExprKind::Binary(_, left, right) => {
            collect_expr_mutations(left, roots);
            collect_expr_mutations(right, roots);
        }
        ExprKind::ReborrowView { owner, .. } => {
            roots.insert(owner.local);
        }
        ExprKind::Constant(_) | ExprKind::Copy(_) | ExprKind::Move(_) => {}
    }
}

fn block_moves_local(block: &Block, local: LocalId) -> bool {
    block
        .statements
        .iter()
        .any(|statement| match &statement.kind {
            StatementKind::Let { value, .. }
            | StatementKind::LetTuple { value, .. }
            | StatementKind::Assign { value, .. }
            | StatementKind::Evaluate(value) => expression_moves_local(value, local),
            StatementKind::ForRange {
                start, end, body, ..
            } => {
                expression_moves_local(start, local)
                    || expression_moves_local(end, local)
                    || block_moves_local(body, local)
            }
            StatementKind::Assert { condition } => expression_moves_local(condition, local),
            StatementKind::Defer(cleanup) => block_moves_local(cleanup, local),
            StatementKind::Return(value) => value
                .as_ref()
                .is_some_and(|value| expression_moves_local(value, local)),
        })
        || block
            .tail
            .as_deref()
            .is_some_and(|tail| expression_moves_local(tail, local))
}

fn call_has_later_move_alias(arguments: &[CallArgument]) -> bool {
    arguments.iter().enumerate().any(|(index, argument)| {
        let CallArgument::InOut(place) = argument else {
            return false;
        };
        arguments[index + 1..].iter().any(|later| {
            matches!(later, CallArgument::Value(value) if expression_moves_local(value, place.local))
        })
    })
}

fn expression_moves_local(expression: &Expr, local: LocalId) -> bool {
    match &expression.kind {
        ExprKind::Move(place) => place.local == local,
        ExprKind::Block(block) => block_moves_local(block, local),
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            expression_moves_local(condition, local)
                || block_moves_local(then_branch, local)
                || block_moves_local(else_branch, local)
        }
        ExprKind::Match { scrutinee, arms } => {
            expression_moves_local(scrutinee, local)
                || arms
                    .iter()
                    .any(|arm| expression_moves_local(&arm.value, local))
        }
        ExprKind::Tuple(values)
        | ExprKind::List(values)
        | ExprKind::Record { fields: values, .. }
        | ExprKind::Variant {
            payload: values, ..
        }
        | ExprKind::TaskJoin {
            arguments: values, ..
        } => values
            .iter()
            .any(|value| expression_moves_local(value, local)),
        ExprKind::Unary(_, value)
        | ExprKind::Refine { value, .. }
        | ExprKind::Unrefine(value)
        | ExprKind::MakeView { value, .. }
        | ExprKind::Await { task: value, .. }
        | ExprKind::Sleep {
            milliseconds: value,
        }
        | ExprKind::WaitFd {
            descriptor: value, ..
        } => expression_moves_local(value, local),
        ExprKind::Binary(_, left, right) => {
            expression_moves_local(left, local) || expression_moves_local(right, local)
        }
        ExprKind::Call { arguments, .. } => arguments.iter().any(|argument| match argument {
            CallArgument::Value(value) => expression_moves_local(value, local),
            CallArgument::InOut(_) => false,
        }),
        ExprKind::Constant(_) | ExprKind::Copy(_) | ExprKind::ReborrowView { .. } => false,
    }
}

fn pattern_bindings(pattern: &Pattern, value: &AbstractValue) -> Option<Vec<AbstractValue>> {
    let mut bindings = Vec::new();
    if bind_pattern(pattern, value, &mut bindings) {
        Some(bindings)
    } else {
        None
    }
}

fn bind_pattern(
    pattern: &Pattern,
    value: &AbstractValue,
    bindings: &mut Vec<AbstractValue>,
) -> bool {
    match pattern {
        Pattern::Wildcard | Pattern::Constant(_) => true,
        Pattern::Binding => {
            bindings.push(value.clone());
            true
        }
        Pattern::Variant {
            ty,
            variant,
            payload,
        } => match value {
            AbstractValue::Enum(value) if value.ty == *ty => {
                let Some(values) = value.variants.get(variant) else {
                    // Range proofs never use variant knowledge as a DCE fact.
                    // A later loop iteration or aliasing call can change the
                    // scrutinee, so retain the arm with unknown bindings.
                    collect_unknown_bindings(pattern, bindings);
                    return true;
                };
                if values.len() != payload.len() {
                    collect_unknown_bindings(pattern, bindings);
                    return true;
                }
                payload
                    .iter()
                    .zip(values)
                    .all(|(pattern, value)| bind_pattern(pattern, value, bindings))
            }
            _ => {
                collect_unknown_bindings(pattern, bindings);
                true
            }
        },
    }
}

fn collect_unknown_bindings(pattern: &Pattern, bindings: &mut Vec<AbstractValue>) {
    match pattern {
        Pattern::Binding => bindings.push(AbstractValue::Top),
        Pattern::Variant { payload, .. } => {
            for pattern in payload {
                collect_unknown_bindings(pattern, bindings);
            }
        }
        Pattern::Wildcard | Pattern::Constant(_) => {}
    }
}

fn remainder_modulus(left: &Expr, right: &Expr, environment: &RangeEnv) -> Option<i64> {
    let ExprKind::Binary(BinaryOp::Multiply, quotient, factor) = &right.kind else {
        return None;
    };
    let ExprKind::Binary(BinaryOp::Divide, numerator, divisor) = &quotient.kind else {
        return None;
    };
    if !same_source(left, numerator) || !same_source(divisor, factor) {
        return None;
    }
    let divisor = source_int_fact(divisor, environment)?;
    if !divisor
        .affine
        .as_ref()
        .is_some_and(|affine| affine.terms.is_empty())
    {
        return None;
    }
    let modulus = divisor.range.exact_value()?;
    (modulus > 0).then_some(modulus)
}

fn same_source(left: &Expr, right: &Expr) -> bool {
    match (&left.kind, &right.kind) {
        (
            ExprKind::Copy(left) | ExprKind::Move(left),
            ExprKind::Copy(right) | ExprKind::Move(right),
        ) => same_place(left, right),
        (ExprKind::Constant(Constant::Int(left)), ExprKind::Constant(Constant::Int(right))) => {
            left == right
        }
        _ => false,
    }
}

fn same_place(left: &Place, right: &Place) -> bool {
    left.local == right.local && left.projection == right.projection
}

fn source_int_fact(expression: &Expr, environment: &RangeEnv) -> Option<IntFact> {
    match &expression.kind {
        ExprKind::Constant(Constant::Int(value)) => Some(IntFact::exact(*value)),
        ExprKind::Copy(place) | ExprKind::Move(place) => match read_place(environment, place) {
            AbstractValue::Int(fact) => Some(fact),
            _ => None,
        },
        _ => None,
    }
}

fn refine_condition(condition: &Expr, truth: bool, environment: &mut RangeEnv) {
    match &condition.kind {
        ExprKind::Unary(UnaryOp::Not, value) => refine_condition(value, !truth, environment),
        // And/Or may run effectful right operands after the left comparison. Replaying either
        // operand's pre-evaluation fact onto the post-condition environment would narrow state
        // mutated by the condition itself. A future path-sensitive transfer may refine these;
        // the closed-world proof plan deliberately does not until then.
        ExprKind::Binary(BinaryOp::And | BinaryOp::Or, ..) => {}
        ExprKind::Binary(operator, left, right) => {
            if let Some((place, bound, operator)) = comparison_place_bound(*operator, left, right) {
                refine_place(&place, bound, operator, truth, environment);
            }
        }
        _ => {}
    }
}

fn comparison_place_bound(
    operator: BinaryOp,
    left: &Expr,
    right: &Expr,
) -> Option<(Place, i64, BinaryOp)> {
    if let (Some(place), ExprKind::Constant(Constant::Int(bound))) =
        (plain_place(left), &right.kind)
    {
        return Some((place.clone(), *bound, operator));
    }
    if let (ExprKind::Constant(Constant::Int(bound)), Some(place)) =
        (&left.kind, plain_place(right))
    {
        let reversed = match operator {
            BinaryOp::Less => BinaryOp::Greater,
            BinaryOp::LessEqual => BinaryOp::GreaterEqual,
            BinaryOp::Greater => BinaryOp::Less,
            BinaryOp::GreaterEqual => BinaryOp::LessEqual,
            other => other,
        };
        return Some((place.clone(), *bound, reversed));
    }
    None
}

fn plain_place(expression: &Expr) -> Option<&Place> {
    match &expression.kind {
        ExprKind::Copy(place) | ExprKind::Move(place) => Some(place),
        _ => None,
    }
}

fn refine_place(
    place: &Place,
    bound: i64,
    operator: BinaryOp,
    truth: bool,
    environment: &mut RangeEnv,
) {
    let current = read_place(environment, place).int_range();
    let allowed = match (operator, truth) {
        (BinaryOp::Equal, true) | (BinaryOp::NotEqual, false) => IntRange::exact(bound),
        (BinaryOp::Less, true) | (BinaryOp::GreaterEqual, false) => {
            IntRange::new(i64::MIN, bound.saturating_sub(1))
        }
        (BinaryOp::LessEqual, true) | (BinaryOp::Greater, false) => IntRange::new(i64::MIN, bound),
        (BinaryOp::Greater, true) | (BinaryOp::LessEqual, false) => {
            IntRange::new(bound.saturating_add(1), i64::MAX)
        }
        (BinaryOp::GreaterEqual, true) | (BinaryOp::Less, false) => IntRange::new(bound, i64::MAX),
        _ => return,
    };
    write_place(
        environment,
        place,
        AbstractValue::Int(IntFact::dependent_range(current.intersect(allowed))),
    );
}

fn block_contains_loop(block: &Block) -> bool {
    block
        .statements
        .iter()
        .any(|statement| match &statement.kind {
            StatementKind::ForRange { .. } => true,
            StatementKind::Defer(body) => block_contains_loop(body),
            StatementKind::Let { value, .. }
            | StatementKind::LetTuple { value, .. }
            | StatementKind::Assign { value, .. }
            | StatementKind::Evaluate(value) => expression_contains_loop(value),
            StatementKind::Assert { condition } => expression_contains_loop(condition),
            StatementKind::Return(value) => value.as_ref().is_some_and(expression_contains_loop),
        })
        || block.tail.as_deref().is_some_and(expression_contains_loop)
}

fn expression_contains_loop(expression: &Expr) -> bool {
    match &expression.kind {
        ExprKind::Block(block) => block_contains_loop(block),
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            expression_contains_loop(condition)
                || block_contains_loop(then_branch)
                || block_contains_loop(else_branch)
        }
        ExprKind::Match { scrutinee, arms } => {
            expression_contains_loop(scrutinee)
                || arms.iter().any(|arm| expression_contains_loop(&arm.value))
        }
        ExprKind::Tuple(values) | ExprKind::List(values) => {
            values.iter().any(expression_contains_loop)
        }
        ExprKind::Record { fields, .. } => fields.iter().any(expression_contains_loop),
        ExprKind::Variant { payload, .. } => payload.iter().any(expression_contains_loop),
        ExprKind::Unary(_, value)
        | ExprKind::Refine { value, .. }
        | ExprKind::Unrefine(value)
        | ExprKind::MakeView { value, .. } => expression_contains_loop(value),
        ExprKind::Binary(_, left, right) => {
            expression_contains_loop(left) || expression_contains_loop(right)
        }
        ExprKind::Call { arguments, .. } => arguments.iter().any(|argument| match argument {
            CallArgument::Value(value) => expression_contains_loop(value),
            CallArgument::InOut(_) => false,
        }),
        ExprKind::Await { task, .. } => expression_contains_loop(task),
        ExprKind::Sleep { milliseconds } => expression_contains_loop(milliseconds),
        ExprKind::WaitFd { descriptor, .. } => expression_contains_loop(descriptor),
        ExprKind::TaskJoin { arguments, .. } => arguments.iter().any(expression_contains_loop),
        ExprKind::Constant(_)
        | ExprKind::Copy(_)
        | ExprKind::Move(_)
        | ExprKind::ReborrowView { .. } => false,
    }
}

fn block_contains_nested_loop(block: &Block) -> bool {
    block_contains_nested_loop_at(block, false)
}

fn block_contains_nested_loop_at(block: &Block, inside_loop: bool) -> bool {
    block
        .statements
        .iter()
        .any(|statement| match &statement.kind {
            StatementKind::ForRange {
                start, end, body, ..
            } => {
                inside_loop
                    || expression_contains_nested_loop_at(start, inside_loop)
                    || expression_contains_nested_loop_at(end, inside_loop)
                    || block_contains_nested_loop_at(body, true)
            }
            StatementKind::Defer(body) => block_contains_nested_loop_at(body, inside_loop),
            StatementKind::Let { value, .. }
            | StatementKind::LetTuple { value, .. }
            | StatementKind::Assign { value, .. }
            | StatementKind::Evaluate(value) => {
                expression_contains_nested_loop_at(value, inside_loop)
            }
            StatementKind::Assert { condition } => {
                expression_contains_nested_loop_at(condition, inside_loop)
            }
            StatementKind::Return(value) => value
                .as_ref()
                .is_some_and(|value| expression_contains_nested_loop_at(value, inside_loop)),
        })
        || block
            .tail
            .as_deref()
            .is_some_and(|tail| expression_contains_nested_loop_at(tail, inside_loop))
}

fn expression_contains_nested_loop_at(expression: &Expr, inside_loop: bool) -> bool {
    match &expression.kind {
        ExprKind::Block(block) => block_contains_nested_loop_at(block, inside_loop),
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            expression_contains_nested_loop_at(condition, inside_loop)
                || block_contains_nested_loop_at(then_branch, inside_loop)
                || block_contains_nested_loop_at(else_branch, inside_loop)
        }
        ExprKind::Match { scrutinee, arms } => {
            expression_contains_nested_loop_at(scrutinee, inside_loop)
                || arms
                    .iter()
                    .any(|arm| expression_contains_nested_loop_at(&arm.value, inside_loop))
        }
        ExprKind::Tuple(values) | ExprKind::List(values) => values
            .iter()
            .any(|value| expression_contains_nested_loop_at(value, inside_loop)),
        ExprKind::Record { fields, .. } => fields
            .iter()
            .any(|field| expression_contains_nested_loop_at(field, inside_loop)),
        ExprKind::Variant { payload, .. } => payload
            .iter()
            .any(|value| expression_contains_nested_loop_at(value, inside_loop)),
        ExprKind::Unary(_, value)
        | ExprKind::Refine { value, .. }
        | ExprKind::Unrefine(value)
        | ExprKind::MakeView { value, .. } => {
            expression_contains_nested_loop_at(value, inside_loop)
        }
        ExprKind::Binary(_, left, right) => {
            expression_contains_nested_loop_at(left, inside_loop)
                || expression_contains_nested_loop_at(right, inside_loop)
        }
        ExprKind::Call { arguments, .. } => arguments.iter().any(|argument| match argument {
            CallArgument::Value(value) => expression_contains_nested_loop_at(value, inside_loop),
            CallArgument::InOut(_) => false,
        }),
        ExprKind::Await { task, .. } => expression_contains_nested_loop_at(task, inside_loop),
        ExprKind::Sleep { milliseconds } => {
            expression_contains_nested_loop_at(milliseconds, inside_loop)
        }
        ExprKind::WaitFd { descriptor, .. } => {
            expression_contains_nested_loop_at(descriptor, inside_loop)
        }
        ExprKind::TaskJoin { arguments, .. } => arguments
            .iter()
            .any(|argument| expression_contains_nested_loop_at(argument, inside_loop)),
        ExprKind::Constant(_)
        | ExprKind::Copy(_)
        | ExprKind::Move(_)
        | ExprKind::ReborrowView { .. } => false,
    }
}

fn block_contains_return(block: &Block) -> bool {
    block
        .statements
        .iter()
        .any(|statement| match &statement.kind {
            StatementKind::Return(_) => true,
            StatementKind::ForRange { body, .. } => block_contains_return(body),
            StatementKind::Defer(body) => block_contains_return(body),
            StatementKind::Let { value, .. }
            | StatementKind::LetTuple { value, .. }
            | StatementKind::Assign { value, .. }
            | StatementKind::Evaluate(value) => expression_contains_return(value),
            StatementKind::Assert { condition } => expression_contains_return(condition),
        })
        || block
            .tail
            .as_deref()
            .is_some_and(expression_contains_return)
}

fn expression_contains_return(expression: &Expr) -> bool {
    match &expression.kind {
        ExprKind::Block(block) => block_contains_return(block),
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            expression_contains_return(condition)
                || block_contains_return(then_branch)
                || block_contains_return(else_branch)
        }
        ExprKind::Match { scrutinee, arms } => {
            expression_contains_return(scrutinee)
                || arms
                    .iter()
                    .any(|arm| expression_contains_return(&arm.value))
        }
        ExprKind::Tuple(values) | ExprKind::List(values) => {
            values.iter().any(expression_contains_return)
        }
        ExprKind::Record { fields, .. } => fields.iter().any(expression_contains_return),
        ExprKind::Variant { payload, .. } => payload.iter().any(expression_contains_return),
        ExprKind::Unary(_, value)
        | ExprKind::Refine { value, .. }
        | ExprKind::Unrefine(value)
        | ExprKind::MakeView { value, .. } => expression_contains_return(value),
        ExprKind::Binary(_, left, right) => {
            expression_contains_return(left) || expression_contains_return(right)
        }
        ExprKind::Call { arguments, .. } => arguments.iter().any(|argument| match argument {
            CallArgument::Value(value) => expression_contains_return(value),
            CallArgument::InOut(_) => false,
        }),
        ExprKind::Await { task, .. } => expression_contains_return(task),
        ExprKind::Sleep { milliseconds } => expression_contains_return(milliseconds),
        ExprKind::WaitFd { descriptor, .. } => expression_contains_return(descriptor),
        ExprKind::TaskJoin { arguments, .. } => arguments.iter().any(expression_contains_return),
        ExprKind::Constant(_)
        | ExprKind::Copy(_)
        | ExprKind::Move(_)
        | ExprKind::ReborrowView { .. } => false,
    }
}

fn block_contains_defer(block: &Block) -> bool {
    block
        .statements
        .iter()
        .any(|statement| match &statement.kind {
            StatementKind::Defer(_) => true,
            StatementKind::ForRange { body, .. } => block_contains_defer(body),
            StatementKind::Let { value, .. }
            | StatementKind::LetTuple { value, .. }
            | StatementKind::Assign { value, .. }
            | StatementKind::Evaluate(value) => expression_contains_defer(value),
            StatementKind::Assert { condition } => expression_contains_defer(condition),
            StatementKind::Return(value) => value.as_ref().is_some_and(expression_contains_defer),
        })
        || block.tail.as_deref().is_some_and(expression_contains_defer)
}

fn expression_contains_defer(expression: &Expr) -> bool {
    match &expression.kind {
        ExprKind::Block(block) => block_contains_defer(block),
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            expression_contains_defer(condition)
                || block_contains_defer(then_branch)
                || block_contains_defer(else_branch)
        }
        ExprKind::Match { scrutinee, arms } => {
            expression_contains_defer(scrutinee)
                || arms.iter().any(|arm| expression_contains_defer(&arm.value))
        }
        ExprKind::Tuple(values) | ExprKind::List(values) => {
            values.iter().any(expression_contains_defer)
        }
        ExprKind::Record { fields, .. } => fields.iter().any(expression_contains_defer),
        ExprKind::Variant { payload, .. } => payload.iter().any(expression_contains_defer),
        ExprKind::Unary(_, value)
        | ExprKind::Refine { value, .. }
        | ExprKind::Unrefine(value)
        | ExprKind::MakeView { value, .. } => expression_contains_defer(value),
        ExprKind::Binary(_, left, right) => {
            expression_contains_defer(left) || expression_contains_defer(right)
        }
        ExprKind::Call { arguments, .. } => arguments.iter().any(|argument| match argument {
            CallArgument::Value(value) => expression_contains_defer(value),
            CallArgument::InOut(_) => false,
        }),
        ExprKind::Await { task, .. } => expression_contains_defer(task),
        ExprKind::Sleep { milliseconds } => expression_contains_defer(milliseconds),
        ExprKind::WaitFd { descriptor, .. } => expression_contains_defer(descriptor),
        ExprKind::TaskJoin { arguments, .. } => arguments.iter().any(expression_contains_defer),
        ExprKind::Constant(_)
        | ExprKind::Copy(_)
        | ExprKind::Move(_)
        | ExprKind::ReborrowView { .. } => false,
    }
}

#[cfg(test)]
#[allow(clippy::default_trait_access)]
mod tests {
    use super::*;

    fn origin(local: u32, kind: OriginKind) -> OriginSite {
        OriginSite {
            function: FunctionId(3),
            place: PlaceSite {
                local: LocalId(local),
                projection: Vec::new(),
            },
            kind,
        }
    }

    fn test_local(id: u32, ty: Type, mutable: bool) -> loom_mir::LocalDecl {
        loom_mir::LocalDecl {
            id: LocalId(id),
            name: format!("local{id}"),
            ty,
            mutable,
            span: Default::default(),
        }
    }

    fn test_function(
        id: u32,
        params: Vec<loom_mir::LocalDecl>,
        locals: Vec<loom_mir::LocalDecl>,
        return_ty: Type,
        body: Block,
    ) -> Function {
        Function {
            id: FunctionId(id),
            name: format!("function{id}"),
            span: Default::default(),
            type_parameters: 0,
            is_async: false,
            suspension_points: Vec::new(),
            params,
            witness_params: Vec::new(),
            witness_prefix_count: 0,
            locals,
            return_ty,
            receiver: None,
            body,
            call_plan: loom_mir::CallPlan::default(),
        }
    }

    fn int(value: i64) -> Expr {
        Expr::new(
            ExprKind::Constant(Constant::Int(value)),
            Type::Int,
            Default::default(),
        )
    }

    fn unit() -> Expr {
        Expr::new(
            ExprKind::Constant(Constant::Unit),
            Type::Unit,
            Default::default(),
        )
    }

    fn copy_int(local: LocalId) -> Expr {
        Expr::new(
            ExprKind::Copy(Place::local(local)),
            Type::Int,
            Default::default(),
        )
    }

    fn binary_int(operator: BinaryOp, left: Expr, right: Expr) -> Expr {
        Expr::new(
            ExprKind::Binary(operator, Box::new(left), Box::new(right)),
            Type::Int,
            Default::default(),
        )
    }

    fn recursive_fibonacci_function() -> Function {
        let parameter = LocalId(0);
        let recursive_call = |offset| {
            Expr::new(
                ExprKind::Call {
                    target: CallTarget::Direct(FunctionId(0)),
                    type_arguments: Vec::new(),
                    arguments: vec![CallArgument::Value(binary_int(
                        BinaryOp::Subtract,
                        copy_int(parameter),
                        int(offset),
                    ))],
                    witnesses: Vec::new(),
                },
                Type::Int,
                Default::default(),
            )
        };
        let mut function = test_function(
            0,
            vec![test_local(0, Type::Int, false)],
            Vec::new(),
            Type::Int,
            Block {
                statements: Vec::new(),
                tail: Some(Box::new(Expr::new(
                    ExprKind::If {
                        condition: Box::new(Expr::new(
                            ExprKind::Binary(
                                BinaryOp::Less,
                                Box::new(copy_int(parameter)),
                                Box::new(int(2)),
                            ),
                            Type::Bool,
                            Default::default(),
                        )),
                        then_branch: Block {
                            statements: Vec::new(),
                            tail: Some(Box::new(copy_int(parameter))),
                            span: Default::default(),
                        },
                        else_branch: Block {
                            statements: Vec::new(),
                            tail: Some(Box::new(binary_int(
                                BinaryOp::Add,
                                recursive_call(1),
                                recursive_call(2),
                            ))),
                            span: Default::default(),
                        },
                    },
                    Type::Int,
                    Default::default(),
                ))),
                span: Default::default(),
            },
        );
        function.renumber_expr_ids().expect("renumber fibonacci");
        function
    }

    #[test]
    fn bounded_recursive_int_plan_proves_fibonacci_through_92_only() {
        let function = recursive_fibonacci_function();
        let expressions = function.exprs_preorder().cloned().collect::<Vec<_>>();
        let program = Program {
            functions: vec![function],
            ..Program::default()
        };
        let reachable = ReachableSourceGraph {
            functions: BTreeSet::from([FunctionId(0)]),
            ..ReachableSourceGraph::default()
        };
        let plan =
            NativeIntRangePlan::analyze(&program, &reachable, &SourceRoots::one(FunctionId(0)));
        let assumed = plan.assumption(FunctionId(0)).expect("assumed fibonacci");
        assert_eq!(assumed.upper(), 92);
        assert_eq!(assumed.exact_result(45), Some(1_134_903_170));
        assert_eq!(assumed.exact_result(92), Some(7_540_113_804_746_346_429));
        assert_eq!(assumed.exact_result(93), None);

        let arithmetic = expressions
            .iter()
            .filter(|expression| {
                matches!(
                    expression.kind,
                    ExprKind::Binary(BinaryOp::Add | BinaryOp::Subtract, _, _)
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(arithmetic.len(), 3);
        assert!(arithmetic.iter().all(|expression| {
            plan.proves_for(NativeBodyMode::Assumed, FunctionId(0), expression)
        }));

        let recursive_calls = expressions
            .iter()
            .filter(|expression| matches!(expression.kind, ExprKind::Call { .. }))
            .collect::<Vec<_>>();
        assert_eq!(recursive_calls.len(), 2);
        assert!(recursive_calls.iter().all(|expression| {
            plan.uses_assumed_call(
                NativeBodyMode::Assumed,
                FunctionId(0),
                expression,
                FunctionId(0),
            )
        }));
        assert!(recursive_calls.iter().all(|expression| {
            !plan.uses_assumed_call(
                NativeBodyMode::Checked,
                FunctionId(0),
                expression,
                FunctionId(0),
            )
        }));
    }

    fn direct_int_call(function: FunctionId, argument: Expr) -> Expr {
        Expr::new(
            ExprKind::Call {
                target: CallTarget::Direct(function),
                type_arguments: Vec::new(),
                arguments: vec![CallArgument::Value(argument)],
                witnesses: Vec::new(),
            },
            Type::Int,
            Default::default(),
        )
    }

    fn select_int(condition: Expr, then_value: i64, else_value: i64) -> Expr {
        Expr::new(
            ExprKind::If {
                condition: Box::new(condition),
                then_branch: Block {
                    statements: Vec::new(),
                    tail: Some(Box::new(int(then_value))),
                    span: Default::default(),
                },
                else_branch: Block {
                    statements: Vec::new(),
                    tail: Some(Box::new(int(else_value))),
                    span: Default::default(),
                },
            },
            Type::Int,
            Default::default(),
        )
    }

    #[test]
    fn assumed_call_sites_require_the_entire_argument_range() {
        let fibonacci = recursive_fibonacci_function();
        let parameter = LocalId(0);
        let negative_condition = || {
            Expr::new(
                ExprKind::Binary(
                    BinaryOp::Less,
                    Box::new(copy_int(parameter)),
                    Box::new(int(0)),
                ),
                Type::Bool,
                Default::default(),
            )
        };
        let arguments = vec![
            select_int(negative_condition(), 0, 45),
            int(92),
            int(0),
            int(-1),
            int(93),
            select_int(negative_condition(), 92, 93),
            copy_int(parameter),
        ];
        let statements = arguments
            .into_iter()
            .map(|argument| loom_mir::Statement {
                kind: StatementKind::Evaluate(direct_int_call(FunctionId(0), argument)),
                span: Default::default(),
            })
            .collect();
        let mut caller = test_function(
            1,
            vec![test_local(0, Type::Int, false)],
            Vec::new(),
            Type::Unit,
            Block {
                statements,
                tail: Some(Box::new(unit())),
                span: Default::default(),
            },
        );
        caller.renumber_expr_ids().expect("renumber caller");
        let calls = caller
            .exprs_preorder()
            .filter(|expression| matches!(expression.kind, ExprKind::Call { .. }))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 7);

        let program = Program {
            functions: vec![fibonacci, caller],
            ..Program::default()
        };
        let reachable = ReachableSourceGraph {
            functions: BTreeSet::from([FunctionId(0), FunctionId(1)]),
            ..ReachableSourceGraph::default()
        };
        let plan =
            NativeIntRangePlan::analyze(&program, &reachable, &SourceRoots::one(FunctionId(1)));
        assert!(calls[..3].iter().all(|expression| {
            plan.uses_assumed_call(
                NativeBodyMode::Checked,
                FunctionId(1),
                expression,
                FunctionId(0),
            )
        }));
        assert!(calls[3..].iter().all(|expression| {
            !plan.uses_assumed_call(
                NativeBodyMode::Checked,
                FunctionId(1),
                expression,
                FunctionId(0),
            )
        }));
    }

    fn single_recursive_function(argument: Expr) -> Function {
        let parameter = LocalId(0);
        let mut function = test_function(
            0,
            vec![test_local(0, Type::Int, false)],
            Vec::new(),
            Type::Int,
            Block {
                statements: Vec::new(),
                tail: Some(Box::new(Expr::new(
                    ExprKind::If {
                        condition: Box::new(Expr::new(
                            ExprKind::Binary(
                                BinaryOp::Less,
                                Box::new(copy_int(parameter)),
                                Box::new(int(1)),
                            ),
                            Type::Bool,
                            Default::default(),
                        )),
                        then_branch: Block {
                            statements: Vec::new(),
                            tail: Some(Box::new(int(0))),
                            span: Default::default(),
                        },
                        else_branch: Block {
                            statements: Vec::new(),
                            tail: Some(Box::new(direct_int_call(FunctionId(0), argument))),
                            span: Default::default(),
                        },
                    },
                    Type::Int,
                    Default::default(),
                ))),
                span: Default::default(),
            },
        );
        function.renumber_expr_ids().expect("renumber recursion");
        function
    }

    #[test]
    fn assumed_solver_rejects_non_decreasing_and_negative_recursion() {
        let parameter = LocalId(0);
        let non_decreasing = single_recursive_function(copy_int(parameter));
        assert!(analyze_assumed_int_function(&non_decreasing).is_none());

        let negative =
            single_recursive_function(binary_int(BinaryOp::Subtract, copy_int(parameter), int(2)));
        assert!(analyze_assumed_int_function(&negative).is_none());
    }

    fn uncovered_arithmetic_function() -> Function {
        let parameter = LocalId(0);
        let inner = Expr::new(
            ExprKind::If {
                condition: Box::new(Expr::new(
                    ExprKind::Binary(
                        BinaryOp::Less,
                        Box::new(copy_int(parameter)),
                        Box::new(int(1)),
                    ),
                    Type::Bool,
                    Default::default(),
                )),
                then_branch: Block {
                    statements: Vec::new(),
                    tail: Some(Box::new(int(0))),
                    span: Default::default(),
                },
                else_branch: Block {
                    statements: Vec::new(),
                    tail: Some(Box::new(direct_int_call(
                        FunctionId(0),
                        binary_int(BinaryOp::Subtract, copy_int(parameter), int(1)),
                    ))),
                    span: Default::default(),
                },
            },
            Type::Int,
            Default::default(),
        );
        let mut function = test_function(
            0,
            vec![test_local(0, Type::Int, false)],
            Vec::new(),
            Type::Int,
            Block {
                statements: Vec::new(),
                tail: Some(Box::new(Expr::new(
                    ExprKind::If {
                        condition: Box::new(Expr::new(
                            ExprKind::Binary(
                                BinaryOp::Less,
                                Box::new(copy_int(parameter)),
                                Box::new(int(129)),
                            ),
                            Type::Bool,
                            Default::default(),
                        )),
                        then_branch: Block {
                            statements: Vec::new(),
                            tail: Some(Box::new(inner)),
                            span: Default::default(),
                        },
                        else_branch: Block {
                            statements: Vec::new(),
                            tail: Some(Box::new(binary_int(BinaryOp::Add, int(i64::MAX), int(1)))),
                            span: Default::default(),
                        },
                    },
                    Type::Int,
                    Default::default(),
                ))),
                span: Default::default(),
            },
        );
        function
            .renumber_expr_ids()
            .expect("renumber uncovered branch");
        function
    }

    #[test]
    fn assumed_solver_rejects_uncovered_checks_contracts_asserts_and_effects() {
        assert!(analyze_assumed_int_function(&uncovered_arithmetic_function()).is_none());

        let mut contracted = recursive_fibonacci_function();
        contracted.call_plan.requires.push(loom_mir::Contract {
            code: "PreconditionFailed".to_owned(),
            span: Default::default(),
            expression: loom_mir::ContractExpr {
                kind: loom_mir::ContractExprKind::Constant(Constant::Bool(true)),
                span: Default::default(),
            },
        });
        assert!(analyze_assumed_int_function(&contracted).is_none());

        let mut asserted = recursive_fibonacci_function();
        asserted.body.statements.push(loom_mir::Statement {
            kind: StatementKind::Assert {
                condition: Expr::new(
                    ExprKind::Constant(Constant::Bool(true)),
                    Type::Bool,
                    Default::default(),
                ),
            },
            span: Default::default(),
        });
        asserted.renumber_expr_ids().expect("renumber assertion");
        assert!(analyze_assumed_int_function(&asserted).is_none());

        let mut effectful = recursive_fibonacci_function();
        effectful.locals.push(test_local(1, Type::Unit, false));
        effectful.body.statements.push(loom_mir::Statement {
            kind: StatementKind::Let {
                local: LocalId(1),
                value: Expr::new(
                    ExprKind::Call {
                        target: CallTarget::Builtin(Builtin::LogInfo),
                        type_arguments: Vec::new(),
                        arguments: vec![CallArgument::Value(Expr::new(
                            ExprKind::Constant(Constant::Text("effect".to_owned())),
                            Type::Text,
                            Default::default(),
                        ))],
                        witnesses: Vec::new(),
                    },
                    Type::Unit,
                    Default::default(),
                ),
            },
            span: Default::default(),
        });
        effectful.renumber_expr_ids().expect("renumber effect");
        assert!(analyze_assumed_int_function(&effectful).is_none());
    }

    #[test]
    fn counted_translation_uses_closed_i128_bounds() {
        let site = origin(1, OriginKind::Integer);
        let base = IntFact::exact(0);
        let step = IntFact {
            range: IntRange::new(0, 1023),
            affine: Some(AffineForm {
                terms: BTreeMap::from([(site.clone(), 1)]),
                residual: IntRange::new(0, 1023),
            }),
        };
        let (before, after) = close_integer_recurrence(&base, &step, &site, 10_000_000)
            .expect("bounded translation closes");
        assert_eq!(before.range, IntRange::new(0, 10_229_998_977));
        assert_eq!(after.range, IntRange::new(0, 10_230_000_000));

        assert!(
            repeated_translation_range(IntRange::exact(i64::MAX), IntRange::exact(1), 1).is_none(),
            "an i128 calculation outside i64 must retain the runtime check"
        );
    }

    #[test]
    fn recurrence_with_another_mutated_origin_is_rejected() {
        let target = origin(1, OriginKind::Integer);
        let other = origin(2, OriginKind::Integer);
        let step = IntFact {
            range: IntRange::new(0, 2),
            affine: Some(AffineForm {
                terms: BTreeMap::from([(target.clone(), 1), (other, 1)]),
                residual: IntRange::exact(0),
            }),
        };
        assert!(
            close_integer_recurrence(&IntFact::exact(0), &step, &target, 100).is_none(),
            "cross-recurrence is not a translation and must become Top"
        );
    }

    #[test]
    fn refinement_does_not_erase_a_mutated_origin_into_an_invariant_delta() {
        let site = origin(1, OriginKind::Integer);
        let mut environment = BTreeMap::from([(
            LocalId(1),
            AbstractValue::Int(IntFact::origin(IntRange::new(0, 20), site.clone())),
        )]);
        refine_place(
            &Place::local(LocalId(1)),
            10,
            BinaryOp::Less,
            true,
            &mut environment,
        );
        let AbstractValue::Int(refined) = read_place(&environment, &Place::local(LocalId(1)))
        else {
            panic!("refined integer must remain an integer");
        };
        assert!(
            refined.affine.is_none(),
            "an origin-dependent refinement is not a loop-invariant residual"
        );

        let step = checked_binary_fact(
            BinaryOp::Add,
            &AbstractValue::Int(IntFact::origin(IntRange::exact(0), site.clone())),
            &AbstractValue::Int(refined),
        )
        .0;
        assert!(
            close_integer_recurrence(&IntFact::exact(0), &step, &site, 3).is_none(),
            "a non-affine self dependency must retain checked arithmetic"
        );
    }

    #[test]
    fn logical_refinement_does_not_replay_pre_effect_facts() {
        let local = LocalId(1);
        let comparison = Expr::new(
            ExprKind::Binary(BinaryOp::Less, Box::new(copy_int(local)), Box::new(int(10))),
            Type::Bool,
            Default::default(),
        );
        let condition = Expr::new(
            ExprKind::Binary(
                BinaryOp::And,
                Box::new(comparison),
                Box::new(Expr::new(
                    ExprKind::Constant(Constant::Bool(true)),
                    Type::Bool,
                    Default::default(),
                )),
            ),
            Type::Bool,
            Default::default(),
        );
        let post_effect = IntRange::new(0, i64::MAX);
        let mut environment = BTreeMap::from([(
            local,
            AbstractValue::Int(IntFact::dependent_range(post_effect)),
        )]);
        refine_condition(&condition, true, &mut environment);
        assert_eq!(
            read_place(&environment, &Place::local(local)).int_range(),
            post_effect,
            "the left comparison happened before a potentially mutating right operand"
        );

        let negated = Expr::new(
            ExprKind::Unary(UnaryOp::Not, Box::new(condition)),
            Type::Bool,
            Default::default(),
        );
        refine_condition(&negated, false, &mut environment);
        assert_eq!(
            read_place(&environment, &Place::local(local)).int_range(),
            post_effect,
            "Not must not re-enable recursive logical refinement"
        );
    }

    #[test]
    fn origin_dependent_remainder_uses_global_signed_bounds_and_is_not_affine() {
        let program = Program::default();
        let mut observations = Vec::new();
        let mut proofs = BTreeMap::new();
        let mut analyzer = FunctionAnalyzer {
            program: &program,
            function_id: FunctionId(0),
            observations: &mut observations,
            proof_visits: &mut proofs,
            record_proofs: false,
            call_stack: BTreeSet::new(),
            conservative_calls: false,
        };
        let local = LocalId(1);
        let mut environment = BTreeMap::from([(
            local,
            AbstractValue::Int(IntFact::origin(
                IntRange::exact(1),
                origin(1, OriginKind::Integer),
            )),
        )]);
        let copy = || {
            Expr::new(
                ExprKind::Copy(Place::local(local)),
                Type::Int,
                Default::default(),
            )
        };
        let ten = || {
            Expr::new(
                ExprKind::Constant(Constant::Int(10)),
                Type::Int,
                Default::default(),
            )
        };
        let quotient = Expr::new(
            ExprKind::Binary(BinaryOp::Divide, Box::new(copy()), Box::new(ten())),
            Type::Int,
            Default::default(),
        );
        let product = Expr::new(
            ExprKind::Binary(BinaryOp::Multiply, Box::new(quotient), Box::new(ten())),
            Type::Int,
            Default::default(),
        );
        let remainder = Expr::new(
            ExprKind::Binary(BinaryOp::Subtract, Box::new(copy()), Box::new(product)),
            Type::Int,
            Default::default(),
        );
        let AbstractValue::Int(result) = analyzer.eval_expr(&remainder, &mut environment) else {
            panic!("canonical remainder must remain an integer");
        };
        assert_eq!(result.range, IntRange::new(-9, 9));
        assert!(
            result.affine.is_none(),
            "a first-iteration sign fact cannot become an invariant recurrence delta"
        );
    }

    #[test]
    fn origin_dependent_modulus_cannot_create_an_invariant_remainder() {
        let program = Program::default();
        let mut observations = Vec::new();
        let mut proofs = BTreeMap::new();
        let mut analyzer = FunctionAnalyzer {
            program: &program,
            function_id: FunctionId(0),
            observations: &mut observations,
            proof_visits: &mut proofs,
            record_proofs: false,
            call_stack: BTreeSet::new(),
            conservative_calls: false,
        };
        let modulus = LocalId(1);
        let mut environment = BTreeMap::from([(
            modulus,
            AbstractValue::Int(IntFact::origin(
                IntRange::exact(10),
                origin(1, OriginKind::Integer),
            )),
        )]);
        let fifty = || {
            Expr::new(
                ExprKind::Constant(Constant::Int(50)),
                Type::Int,
                Default::default(),
            )
        };
        let divisor = || {
            Expr::new(
                ExprKind::Copy(Place::local(modulus)),
                Type::Int,
                Default::default(),
            )
        };
        let quotient = Expr::new(
            ExprKind::Binary(BinaryOp::Divide, Box::new(fifty()), Box::new(divisor())),
            Type::Int,
            Default::default(),
        );
        let product = Expr::new(
            ExprKind::Binary(BinaryOp::Multiply, Box::new(quotient), Box::new(divisor())),
            Type::Int,
            Default::default(),
        );
        let remainder = Expr::new(
            ExprKind::Binary(BinaryOp::Subtract, Box::new(fifty()), Box::new(product)),
            Type::Int,
            Default::default(),
        );
        let AbstractValue::Int(result) = analyzer.eval_expr(&remainder, &mut environment) else {
            panic!("canonical remainder must remain an integer");
        };
        assert!(
            result.affine.is_none(),
            "a modulus mutated by the loop cannot become an invariant recurrence delta"
        );
    }

    #[test]
    fn closed_transfer_rejects_a_later_iteration_modulus_escape() {
        let modulus = LocalId(0);
        let accumulator = LocalId(1);
        let iteration = LocalId(2);
        let leading_add = binary_int(BinaryOp::Add, copy_int(accumulator), int(i64::MAX - 18));
        let quotient = binary_int(BinaryOp::Divide, int(50), copy_int(modulus));
        let product = binary_int(BinaryOp::Multiply, quotient, copy_int(modulus));
        let remainder = binary_int(BinaryOp::Subtract, int(50), product);
        let advance = binary_int(BinaryOp::Add, copy_int(accumulator), remainder);
        let loop_body = Block {
            statements: vec![
                loom_mir::Statement {
                    kind: StatementKind::Evaluate(leading_add),
                    span: Default::default(),
                },
                loom_mir::Statement {
                    kind: StatementKind::Assign {
                        place: Place::local(accumulator),
                        value: advance,
                    },
                    span: Default::default(),
                },
                loom_mir::Statement {
                    kind: StatementKind::Assign {
                        place: Place::local(modulus),
                        value: int(100),
                    },
                    span: Default::default(),
                },
            ],
            tail: Some(Box::new(unit())),
            span: Default::default(),
        };
        let mut function = test_function(
            0,
            Vec::new(),
            vec![
                test_local(0, Type::Int, true),
                test_local(1, Type::Int, true),
                test_local(2, Type::Int, false),
            ],
            Type::Unit,
            Block {
                statements: vec![
                    loom_mir::Statement {
                        kind: StatementKind::Let {
                            local: modulus,
                            value: int(10),
                        },
                        span: Default::default(),
                    },
                    loom_mir::Statement {
                        kind: StatementKind::Let {
                            local: accumulator,
                            value: int(0),
                        },
                        span: Default::default(),
                    },
                    loom_mir::Statement {
                        kind: StatementKind::ForRange {
                            local: iteration,
                            start: Box::new(int(0)),
                            end: Box::new(int(3)),
                            body: Box::new(loop_body),
                        },
                        span: Default::default(),
                    },
                ],
                tail: Some(Box::new(unit())),
                span: Default::default(),
            },
        );
        function.renumber_expr_ids().expect("renumber function");
        let leading_add = function
            .exprs_preorder()
            .find(|expression| {
                matches!(
                    &expression.kind,
                    ExprKind::Binary(BinaryOp::Add, _, right)
                        if matches!(right.kind, ExprKind::Constant(Constant::Int(value)) if value == i64::MAX - 18)
                )
            })
            .expect("leading checked addition")
            .clone();
        let program = Program {
            functions: vec![function],
            ..Program::default()
        };
        let reachable = ReachableSourceGraph {
            functions: BTreeSet::from([FunctionId(0)]),
            ..ReachableSourceGraph::default()
        };
        let plan =
            NativeIntRangePlan::analyze(&program, &reachable, &SourceRoots::one(FunctionId(0)));
        assert!(
            !plan.proves(FunctionId(0), &leading_add),
            "a later modulus state reaches 50 and must retain the MAX-18 + 50 check"
        );
    }

    #[test]
    fn mutable_induction_binding_is_fail_closed_for_unchecked_callers() {
        let iteration = LocalId(0);
        let subtraction = binary_int(BinaryOp::Subtract, copy_int(iteration), int(1));
        let mut function = test_function(
            0,
            Vec::new(),
            vec![test_local(0, Type::Int, true)],
            Type::Unit,
            Block {
                statements: vec![loom_mir::Statement {
                    kind: StatementKind::ForRange {
                        local: iteration,
                        start: Box::new(int(0)),
                        end: Box::new(int(2)),
                        body: Box::new(Block {
                            statements: vec![
                                loom_mir::Statement {
                                    kind: StatementKind::Evaluate(subtraction),
                                    span: Default::default(),
                                },
                                loom_mir::Statement {
                                    kind: StatementKind::Assign {
                                        place: Place::local(iteration),
                                        value: int(i64::MAX),
                                    },
                                    span: Default::default(),
                                },
                            ],
                            tail: Some(Box::new(unit())),
                            span: Default::default(),
                        }),
                    },
                    span: Default::default(),
                }],
                tail: Some(Box::new(unit())),
                span: Default::default(),
            },
        );
        function.renumber_expr_ids().expect("renumber function");
        let subtraction = function
            .exprs_preorder()
            .find(|expression| matches!(expression.kind, ExprKind::Binary(BinaryOp::Subtract, ..)))
            .expect("loop subtraction")
            .clone();
        let program = Program {
            functions: vec![function],
            ..Program::default()
        };
        let reachable = ReachableSourceGraph {
            functions: BTreeSet::from([FunctionId(0)]),
            ..ReachableSourceGraph::default()
        };
        let plan =
            NativeIntRangePlan::analyze(&program, &reachable, &SourceRoots::one(FunctionId(0)));
        assert!(
            !plan.proves(FunctionId(0), &subtraction),
            "an unchecked mutable loop binding must never inherit the static range interval"
        );
    }

    #[test]
    fn list_closure_preserves_element_envelope_and_counted_length() {
        let list_origin = origin(7, OriginKind::ListLength);
        let base = AbstractValue::IntList(ListFacts {
            element: None,
            element_stable: true,
            length: IntFact::exact(0),
        });
        let step = AbstractValue::IntList(ListFacts {
            element: Some(IntRange::new(0, 1023)),
            element_stable: true,
            length: IntFact {
                range: IntRange::exact(1),
                affine: Some(AffineForm {
                    terms: BTreeMap::from([(list_origin, 1)]),
                    residual: IntRange::exact(1),
                }),
            },
        });
        let (before, after) = close_recurrence(
            &base,
            &step,
            FunctionId(3),
            &PlaceSite {
                local: LocalId(7),
                projection: Vec::new(),
            },
            10_000_000,
        );
        let AbstractValue::IntList(before) = before else {
            panic!("list pre-state must remain precise");
        };
        let AbstractValue::IntList(after) = after else {
            panic!("list post-state must remain precise");
        };
        assert_eq!(before.element, Some(IntRange::new(0, 1023)));
        assert_eq!(before.length.range, IntRange::new(0, 9_999_999));
        assert_eq!(after.element, Some(IntRange::new(0, 1023)));
        assert_eq!(after.length.range, IntRange::new(0, 10_000_000));
    }

    #[test]
    fn unknown_list_get_keeps_both_match_outcomes_reachable() {
        let option = TypeId(9);
        let program = Program {
            prelude: loom_mir::PreludeIds {
                option: Some(option),
                ..loom_mir::PreludeIds::default()
            },
            ..Program::default()
        };
        let mut observations = Vec::new();
        let mut proofs = BTreeMap::new();
        let mut analyzer = FunctionAnalyzer {
            program: &program,
            function_id: FunctionId(0),
            observations: &mut observations,
            proof_visits: &mut proofs,
            record_proofs: false,
            call_stack: BTreeSet::new(),
            conservative_calls: false,
        };
        let mut environment = RangeEnv::new();
        let result = analyzer.eval_builtin(
            Builtin::ListGet,
            &[AbstractValue::Top, AbstractValue::Int(IntFact::exact(0))],
            &[None, None],
            &Type::Nominal(option, Vec::new()),
            &mut environment,
        );
        assert_eq!(result, AbstractValue::Top);
    }

    #[test]
    fn short_circuit_rhs_mutation_is_joined_with_the_skipped_state() {
        let program = Program::default();
        let mut observations = Vec::new();
        let mut proofs = BTreeMap::new();
        let mut analyzer = FunctionAnalyzer {
            program: &program,
            function_id: FunctionId(0),
            observations: &mut observations,
            proof_visits: &mut proofs,
            record_proofs: false,
            call_stack: BTreeSet::new(),
            conservative_calls: false,
        };
        let mut environment =
            BTreeMap::from([(LocalId(0), AbstractValue::Int(IntFact::exact(i64::MAX)))]);
        let rhs = Expr::new(
            ExprKind::Block(Block {
                statements: vec![loom_mir::Statement {
                    kind: StatementKind::Assign {
                        place: Place::local(LocalId(0)),
                        value: Expr::new(
                            ExprKind::Constant(Constant::Int(0)),
                            Type::Int,
                            Default::default(),
                        ),
                    },
                    span: Default::default(),
                }],
                tail: Some(Box::new(Expr::new(
                    ExprKind::Constant(Constant::Bool(true)),
                    Type::Bool,
                    Default::default(),
                ))),
                span: Default::default(),
            }),
            Type::Bool,
            Default::default(),
        );
        let logical = Expr::new(
            ExprKind::Binary(
                BinaryOp::And,
                Box::new(Expr::new(
                    ExprKind::Constant(Constant::Bool(false)),
                    Type::Bool,
                    Default::default(),
                )),
                Box::new(rhs),
            ),
            Type::Bool,
            Default::default(),
        );
        analyzer.eval_expr(&logical, &mut environment);
        assert_eq!(
            read_place(&environment, &Place::local(LocalId(0))).int_range(),
            IntRange::new(0, i64::MAX)
        );
    }

    #[test]
    fn non_recurrent_list_elements_expand_without_widening_closed_length() {
        let current = AbstractValue::IntList(ListFacts {
            element: Some(IntRange::exact(0)),
            element_stable: true,
            length: IntFact::range(IntRange::new(0, 2)),
        });
        let transferred = AbstractValue::IntList(ListFacts {
            element: Some(IntRange::FULL),
            element_stable: false,
            length: IntFact::range(IntRange::new(1, 3)),
        });
        let AbstractValue::IntList(joined) = join_non_recurrent_envelopes(&current, &transferred)
        else {
            panic!("list envelope must remain a list");
        };
        assert_eq!(joined.element, Some(IntRange::FULL));
        assert_eq!(joined.length.range, IntRange::new(0, 2));
    }

    #[test]
    fn inout_value_is_observed_after_later_argument_effects() {
        let program = Program::default();
        let mut observations = Vec::new();
        let mut proofs = BTreeMap::new();
        let mut analyzer = FunctionAnalyzer {
            program: &program,
            function_id: FunctionId(0),
            observations: &mut observations,
            proof_visits: &mut proofs,
            record_proofs: false,
            call_stack: BTreeSet::new(),
            conservative_calls: false,
        };
        let list = LocalId(0);
        let mut environment = BTreeMap::from([(
            list,
            AbstractValue::IntList(ListFacts {
                element: None,
                element_stable: true,
                length: IntFact::exact(0),
            }),
        )]);
        let inner_add = Expr::new(
            ExprKind::Call {
                target: CallTarget::Builtin(Builtin::ListAdd),
                type_arguments: Vec::new(),
                arguments: vec![
                    CallArgument::InOut(Place::local(list)),
                    CallArgument::Value(Expr::new(
                        ExprKind::Constant(Constant::Int(1)),
                        Type::Int,
                        Default::default(),
                    )),
                ],
                witnesses: Vec::new(),
            },
            Type::Unit,
            Default::default(),
        );
        let second_element = Expr::new(
            ExprKind::Block(Block {
                statements: vec![loom_mir::Statement {
                    kind: StatementKind::Evaluate(inner_add),
                    span: Default::default(),
                }],
                tail: Some(Box::new(Expr::new(
                    ExprKind::Constant(Constant::Int(2)),
                    Type::Int,
                    Default::default(),
                ))),
                span: Default::default(),
            }),
            Type::Int,
            Default::default(),
        );
        analyzer.eval_call(
            &CallTarget::Builtin(Builtin::ListAdd),
            &[
                CallArgument::InOut(Place::local(list)),
                CallArgument::Value(second_element),
            ],
            false,
            &Type::Unit,
            &mut environment,
            None,
        );
        let AbstractValue::IntList(list) = environment.get(&list).expect("list remains bound")
        else {
            panic!("list facts must survive nested add");
        };
        assert_eq!(list.element, Some(IntRange::new(1, 2)));
        assert_eq!(list.length.range, IntRange::exact(2));
    }

    #[test]
    fn projected_inout_is_unknown_until_codegen_defers_projection_resolution() {
        let parameter = test_local(0, Type::Int, true);
        let helper = test_function(
            0,
            vec![parameter],
            Vec::new(),
            Type::Unit,
            Block {
                statements: Vec::new(),
                tail: Some(Box::new(unit())),
                span: Default::default(),
            },
        );
        let program = Program {
            functions: vec![helper],
            ..Program::default()
        };
        let root = LocalId(7);
        let projected = Place {
            local: root,
            projection: vec![0],
        };
        let mut environment = BTreeMap::from([(
            root,
            AbstractValue::Record {
                ty: TypeId(9),
                fields: vec![AbstractValue::Int(IntFact::exact(i64::MAX))],
            },
        )]);
        let mut observations = Vec::new();
        let mut proofs = BTreeMap::new();
        {
            let mut analyzer = FunctionAnalyzer {
                program: &program,
                function_id: FunctionId(1),
                observations: &mut observations,
                proof_visits: &mut proofs,
                record_proofs: false,
                call_stack: BTreeSet::from([FunctionId(1)]),
                conservative_calls: false,
            };
            analyzer.eval_call(
                &CallTarget::Direct(FunctionId(0)),
                &[CallArgument::InOut(projected)],
                false,
                &Type::Unit,
                &mut environment,
                None,
            );
        }
        assert!(
            matches!(observations[0].arguments[0], AbstractValue::Top),
            "a projected callee must not inherit the replacement root's field fact"
        );
        assert!(
            matches!(environment.get(&root), Some(AbstractValue::Top)),
            "a detached projected writeback cannot define the replacement root's post-state"
        );
    }

    #[test]
    fn inout_aliasing_a_later_move_forces_an_unknown_callee_context() {
        let helper = test_function(
            0,
            vec![
                test_local(0, Type::Int, true),
                test_local(1, Type::Int, false),
            ],
            Vec::new(),
            Type::Unit,
            Block {
                statements: Vec::new(),
                tail: Some(Box::new(unit())),
                span: Default::default(),
            },
        );
        let program = Program {
            functions: vec![helper],
            ..Program::default()
        };
        let root = LocalId(7);
        let mut environment =
            BTreeMap::from([(root, AbstractValue::Int(IntFact::exact(i64::MAX)))]);
        let mut observations = Vec::new();
        let mut proofs = BTreeMap::new();
        {
            let mut analyzer = FunctionAnalyzer {
                program: &program,
                function_id: FunctionId(1),
                observations: &mut observations,
                proof_visits: &mut proofs,
                record_proofs: false,
                call_stack: BTreeSet::from([FunctionId(1)]),
                conservative_calls: false,
            };
            analyzer.eval_call(
                &CallTarget::Direct(FunctionId(0)),
                &[
                    CallArgument::InOut(Place::local(root)),
                    CallArgument::Value(Expr::new(
                        ExprKind::Move(Place::local(root)),
                        Type::Int,
                        Default::default(),
                    )),
                ],
                false,
                &Type::Unit,
                &mut environment,
                None,
            );
        }
        assert!(
            observations[0]
                .arguments
                .iter()
                .all(|argument| argument.int_range() == IntRange::FULL),
            "an aliased move makes every callee parameter context unknown"
        );
        assert!(matches!(environment.get(&root), Some(AbstractValue::Top)));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn defer_call_propagates_unknown_context_to_its_helper() {
        let parameter = test_local(0, Type::Int, false);
        let mut helper = test_function(
            0,
            vec![parameter],
            Vec::new(),
            Type::Int,
            Block {
                statements: Vec::new(),
                tail: Some(Box::new(Expr::new(
                    ExprKind::Binary(
                        BinaryOp::Add,
                        Box::new(Expr::new(
                            ExprKind::Copy(Place::local(LocalId(0))),
                            Type::Int,
                            Default::default(),
                        )),
                        Box::new(Expr::new(
                            ExprKind::Constant(Constant::Int(1)),
                            Type::Int,
                            Default::default(),
                        )),
                    ),
                    Type::Int,
                    Default::default(),
                ))),
                span: Default::default(),
            },
        );
        helper.renumber_expr_ids().expect("renumber helper");
        let helper_add = helper
            .exprs_preorder()
            .find(|expression| matches!(expression.kind, ExprKind::Binary(BinaryOp::Add, ..)))
            .expect("helper addition")
            .clone();

        let local = test_local(0, Type::Int, true);
        let cleanup_call = Expr::new(
            ExprKind::Call {
                target: CallTarget::Direct(FunctionId(0)),
                type_arguments: Vec::new(),
                arguments: vec![CallArgument::Value(Expr::new(
                    ExprKind::Copy(Place::local(LocalId(0))),
                    Type::Int,
                    Default::default(),
                ))],
                witnesses: Vec::new(),
            },
            Type::Int,
            Default::default(),
        );
        let mut caller = test_function(
            1,
            Vec::new(),
            vec![local],
            Type::Unit,
            Block {
                statements: vec![
                    loom_mir::Statement {
                        kind: StatementKind::Let {
                            local: LocalId(0),
                            value: Expr::new(
                                ExprKind::Constant(Constant::Int(0)),
                                Type::Int,
                                Default::default(),
                            ),
                        },
                        span: Default::default(),
                    },
                    loom_mir::Statement {
                        kind: StatementKind::Defer(Block {
                            statements: vec![loom_mir::Statement {
                                kind: StatementKind::Evaluate(cleanup_call),
                                span: Default::default(),
                            }],
                            tail: Some(Box::new(Expr::new(
                                ExprKind::Constant(Constant::Unit),
                                Type::Unit,
                                Default::default(),
                            ))),
                            span: Default::default(),
                        }),
                        span: Default::default(),
                    },
                    loom_mir::Statement {
                        kind: StatementKind::Assign {
                            place: Place::local(LocalId(0)),
                            value: Expr::new(
                                ExprKind::Constant(Constant::Int(i64::MAX)),
                                Type::Int,
                                Default::default(),
                            ),
                        },
                        span: Default::default(),
                    },
                ],
                tail: Some(Box::new(Expr::new(
                    ExprKind::Constant(Constant::Unit),
                    Type::Unit,
                    Default::default(),
                ))),
                span: Default::default(),
            },
        );
        caller.renumber_expr_ids().expect("renumber caller");
        let program = Program {
            functions: vec![helper, caller],
            ..Program::default()
        };
        let reachable = ReachableSourceGraph {
            functions: BTreeSet::from([FunctionId(0), FunctionId(1)]),
            ..ReachableSourceGraph::default()
        };
        let plan =
            NativeIntRangePlan::analyze(&program, &reachable, &SourceRoots::one(FunctionId(1)));
        assert!(
            !plan.proves(FunctionId(0), &helper_add),
            "a deferred helper must retain the MAX + 1 overflow check"
        );
    }
}
