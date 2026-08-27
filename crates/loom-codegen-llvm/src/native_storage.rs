//! Closed-world selection of compiler-private native storage.
//!
//! These plans are deliberately conservative. A local is selected only when
//! every use in its synchronous MIR body can be lowered without ever exposing
//! the private representation through the universal `Value` ABI.

use std::collections::{BTreeMap, BTreeSet};

use loom_mir::{
    Block, Builtin, CallArgument, CallTarget, Constant, ConstructionMode, Expr, ExprKind, Function,
    LocalId, MatchArm, Pattern, Place, Program, StatementKind, Type, TypeId,
};

use crate::native_layout::{NativeLayout, NativePodRecord, NativeSignatureShape};

const MAX_STACK_RECORD_FIELDS: usize = 16;
const MAX_STACK_RECORD_NODES_PER_FUNCTION: usize = 64;

/// Returns the private source local for the only POD value argument shape currently admitted by
/// both requirement analysis and emission. Keeping this syntax decision shared prevents either
/// phase from silently selecting a different calling convention.
#[must_use]
pub(crate) fn native_pod_value_argument_local(argument: &CallArgument) -> Option<LocalId> {
    let CallArgument::Value(Expr {
        kind: ExprKind::Copy(place),
        ..
    }) = argument
    else {
        return None;
    };
    place.projection.is_empty().then_some(place.local)
}

/// Compiler-private stack nodes for a bounded POD record local.
///
/// Eligibility and initializer recognition live in this shared plan so the
/// requirement graph and the LLVM emitter cannot disagree about which record
/// containers avoid managed allocation.
#[derive(Clone, Debug, Default)]
pub(crate) struct NativeStackRecordPlan {
    candidates: BTreeMap<LocalId, StackRecordCandidate>,
    native_pod_initializers: BTreeMap<loom_mir::FunctionId, TypeId>,
}

#[derive(Clone, Copy, Debug)]
struct StackRecordCandidate {
    nominal: TypeId,
    fields: usize,
}

impl NativeStackRecordPlan {
    #[must_use]
    pub(crate) fn analyze(program: &Program, function: &Function) -> Self {
        if function.is_async {
            return Self::default();
        }
        let eligible = function
            .locals
            .iter()
            .filter_map(|local| {
                let NativeLayout::PodRecord(record) = NativeLayout::classify(program, &local.ty)?
                else {
                    return None;
                };
                if record.fields().len() > MAX_STACK_RECORD_FIELDS {
                    return None;
                }
                Some((local.id, record))
            })
            .collect::<BTreeMap<_, _>>();
        if eligible.is_empty() {
            return Self::default();
        }
        let native_pod_initializers = program
            .functions
            .iter()
            .filter_map(|function| {
                let shape = NativeSignatureShape::for_supported_function(program, function)?;
                let NativeLayout::PodRecord(result) = shape.result() else {
                    return None;
                };
                Some((function.id, result.nominal()))
            })
            .collect::<BTreeMap<_, _>>();
        let mut initialized = BTreeMap::<LocalId, usize>::new();
        let mut forbidden = BTreeSet::new();
        scan_stack_record_block(
            &function.body,
            &eligible,
            &native_pod_initializers,
            &mut initialized,
            &mut forbidden,
        );
        let mut total_nodes = 0_usize;
        let candidates = eligible
            .into_iter()
            .filter_map(|(local, record)| {
                if initialized.get(&local) != Some(&1) || forbidden.contains(&local) {
                    return None;
                }
                let fields = record.fields().len();
                let next_total = total_nodes.checked_add(fields)?;
                if next_total > MAX_STACK_RECORD_NODES_PER_FUNCTION {
                    return None;
                }
                total_nodes = next_total;
                Some((
                    local,
                    StackRecordCandidate {
                        nominal: record.nominal(),
                        fields,
                    },
                ))
            })
            .collect();
        Self {
            candidates,
            native_pod_initializers,
        }
    }

    #[must_use]
    pub(crate) fn contains(&self, local: LocalId) -> bool {
        self.candidates.contains_key(&local)
    }

    pub(crate) fn locals(&self) -> impl Iterator<Item = (LocalId, usize)> + '_ {
        self.candidates
            .iter()
            .map(|(local, candidate)| (*local, candidate.fields))
    }

    #[must_use]
    pub(crate) fn is_initializer(&self, local: LocalId, expression: &Expr) -> bool {
        self.candidates.get(&local).is_some_and(|candidate| {
            is_stack_record_initializer(
                expression,
                candidate.nominal,
                &self.native_pod_initializers,
            )
        })
    }
}

fn is_stack_record_initializer(
    expression: &Expr,
    expected: TypeId,
    native_pod_initializers: &BTreeMap<loom_mir::FunctionId, TypeId>,
) -> bool {
    match &expression.kind {
        ExprKind::Record {
            ty,
            construction: ConstructionMode::Plain | ConstructionMode::Proven,
            ..
        } => *ty == expected,
        ExprKind::Block(block) => block.tail.as_deref().is_some_and(|tail| {
            is_stack_record_initializer(tail, expected, native_pod_initializers)
        }),
        ExprKind::Call {
            target: CallTarget::Direct(function) | CallTarget::Inherent(function),
            type_arguments,
            witnesses,
            ..
        } => {
            type_arguments.is_empty()
                && witnesses.is_empty()
                && native_pod_initializers.get(function) == Some(&expected)
        }
        _ => false,
    }
}

fn scan_stack_record_block(
    block: &Block,
    eligible: &BTreeMap<LocalId, NativePodRecord>,
    native_pod_initializers: &BTreeMap<loom_mir::FunctionId, TypeId>,
    initialized: &mut BTreeMap<LocalId, usize>,
    forbidden: &mut BTreeSet<LocalId>,
) {
    for statement in &block.statements {
        scan_stack_record_statement(
            &statement.kind,
            eligible,
            native_pod_initializers,
            initialized,
            forbidden,
        );
    }
    if let Some(tail) = &block.tail {
        scan_stack_record_expr(
            tail,
            eligible,
            native_pod_initializers,
            initialized,
            forbidden,
        );
    }
}

fn scan_stack_record_statement(
    statement: &StatementKind,
    eligible: &BTreeMap<LocalId, NativePodRecord>,
    native_pod_initializers: &BTreeMap<loom_mir::FunctionId, TypeId>,
    initialized: &mut BTreeMap<LocalId, usize>,
    forbidden: &mut BTreeSet<LocalId>,
) {
    macro_rules! scan_expr {
        ($value:expr) => {
            scan_stack_record_expr(
                $value,
                eligible,
                native_pod_initializers,
                initialized,
                forbidden,
            )
        };
    }
    match statement {
        StatementKind::Let { local, value } => {
            if let Some(record) = eligible.get(local) {
                if is_stack_record_initializer(value, record.nominal(), native_pod_initializers) {
                    *initialized.entry(*local).or_default() += 1;
                } else {
                    forbidden.insert(*local);
                }
            }
            scan_expr!(value);
        }
        StatementKind::Scoped { local, value, .. } => {
            if eligible.contains_key(local) {
                forbidden.insert(*local);
            }
            scan_expr!(value);
        }
        StatementKind::LetTuple { locals, value } => {
            forbidden.extend(
                locals
                    .iter()
                    .copied()
                    .filter(|local| eligible.contains_key(local)),
            );
            scan_expr!(value);
        }
        StatementKind::ForRange {
            start, end, body, ..
        } => {
            scan_expr!(start);
            scan_expr!(end);
            scan_stack_record_block(
                body,
                eligible,
                native_pod_initializers,
                initialized,
                forbidden,
            );
        }
        StatementKind::Assign { place, value } => {
            if place.projection.is_empty() && eligible.contains_key(&place.local) {
                forbidden.insert(place.local);
            }
            scan_expr!(value);
        }
        StatementKind::Assert { condition } | StatementKind::Evaluate(condition) => {
            scan_expr!(condition);
        }
        StatementKind::Defer(cleanup) => scan_stack_record_block(
            cleanup,
            eligible,
            native_pod_initializers,
            initialized,
            forbidden,
        ),
        StatementKind::Return(value) => {
            if let Some(value) = value {
                scan_expr!(value);
            }
        }
    }
}

#[allow(clippy::too_many_lines)]
fn scan_stack_record_expr(
    expression: &Expr,
    eligible: &BTreeMap<LocalId, NativePodRecord>,
    native_pod_initializers: &BTreeMap<loom_mir::FunctionId, TypeId>,
    initialized: &mut BTreeMap<LocalId, usize>,
    forbidden: &mut BTreeSet<LocalId>,
) {
    match &expression.kind {
        ExprKind::Constant(_) | ExprKind::Copy(_) => {}
        ExprKind::Move(place) => {
            if eligible.contains_key(&place.local) {
                forbidden.insert(place.local);
            }
        }
        ExprKind::Tuple(values)
        | ExprKind::List(values)
        | ExprKind::TaskJoin {
            arguments: values, ..
        } => {
            for value in values {
                scan_stack_record_expr(
                    value,
                    eligible,
                    native_pod_initializers,
                    initialized,
                    forbidden,
                );
            }
        }
        ExprKind::Unary(_, value)
        | ExprKind::Unrefine(value)
        | ExprKind::Refine { value, .. }
        | ExprKind::Await { task: value, .. }
        | ExprKind::Sleep {
            milliseconds: value,
        } => scan_stack_record_expr(
            value,
            eligible,
            native_pod_initializers,
            initialized,
            forbidden,
        ),
        ExprKind::Binary(_, left, right) => {
            scan_stack_record_expr(
                left,
                eligible,
                native_pod_initializers,
                initialized,
                forbidden,
            );
            scan_stack_record_expr(
                right,
                eligible,
                native_pod_initializers,
                initialized,
                forbidden,
            );
        }
        ExprKind::Block(block) => {
            scan_stack_record_block(
                block,
                eligible,
                native_pod_initializers,
                initialized,
                forbidden,
            );
        }
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            scan_stack_record_expr(
                condition,
                eligible,
                native_pod_initializers,
                initialized,
                forbidden,
            );
            scan_stack_record_block(
                then_branch,
                eligible,
                native_pod_initializers,
                initialized,
                forbidden,
            );
            scan_stack_record_block(
                else_branch,
                eligible,
                native_pod_initializers,
                initialized,
                forbidden,
            );
        }
        ExprKind::Match { scrutinee, arms } => {
            scan_stack_record_expr(
                scrutinee,
                eligible,
                native_pod_initializers,
                initialized,
                forbidden,
            );
            for arm in arms {
                scan_stack_record_expr(
                    &arm.value,
                    eligible,
                    native_pod_initializers,
                    initialized,
                    forbidden,
                );
            }
        }
        ExprKind::Record { fields, .. } => {
            for field in fields {
                scan_stack_record_expr(
                    field,
                    eligible,
                    native_pod_initializers,
                    initialized,
                    forbidden,
                );
            }
        }
        ExprKind::Variant { payload, .. } => {
            for value in payload {
                scan_stack_record_expr(
                    value,
                    eligible,
                    native_pod_initializers,
                    initialized,
                    forbidden,
                );
            }
        }
        ExprKind::Call { arguments, .. } => {
            for argument in arguments {
                match argument {
                    CallArgument::Value(value) => {
                        scan_stack_record_expr(
                            value,
                            eligible,
                            native_pod_initializers,
                            initialized,
                            forbidden,
                        );
                    }
                    CallArgument::InOut(_) => {
                        // Checked MIR makes an InOut loan call-scoped. Callees
                        // can mutate the value, but every observable escape is
                        // a deep value copy, so the backing nodes cannot outlive
                        // this synchronous frame.
                    }
                }
            }
        }
        ExprKind::MakeView {
            value, writeback, ..
        } => {
            scan_stack_record_expr(
                value,
                eligible,
                native_pod_initializers,
                initialized,
                forbidden,
            );
            if let Some(place) = writeback
                && eligible.contains_key(&place.local)
            {
                forbidden.insert(place.local);
            }
        }
        ExprKind::ReborrowView { owner, .. } => {
            if eligible.contains_key(&owner.local) {
                forbidden.insert(owner.local);
            }
        }
    }
}

/// Native, uniquely-owned `{ data, len, capacity }` storage for local
/// `List[Int]` values.
#[derive(Clone, Debug, Default)]
pub(crate) struct NativeIntListPlan {
    locals: BTreeSet<LocalId>,
    option: Option<TypeId>,
    /// `(range binding, list local)` pairs for which every direct
    /// `list.get(range_binding)` is statically in bounds. The proof is
    /// compiler-private and never changes `List.get` semantics for a shape
    /// which does not satisfy the exact-length analysis below.
    proven_exhaustive_gets: BTreeSet<(LocalId, LocalId)>,
}

impl NativeIntListPlan {
    #[must_use]
    pub(crate) fn analyze(program: &Program, function: &Function) -> Self {
        if function.is_async {
            return Self::default();
        }
        let option = program.prelude.option;
        let mut locals = BTreeSet::new();
        let mut proven_exhaustive_gets = BTreeSet::new();
        for local in function
            .locals
            .iter()
            .filter(|local| local.ty == Type::List(Box::new(Type::Int)))
        {
            let mut scanner = IntListUseScanner::new(local.id, option);
            scanner.scan_block(&function.body, true);
            if scanner.valid && scanner.initializers == 1 {
                locals.insert(local.id);
                if let Some(option) = option {
                    proven_exhaustive_gets.extend(prove_exhaustive_int_list_gets(
                        function,
                        option,
                        local.id,
                        &scanner.range_binding_counts,
                    ));
                }
            }
        }
        Self {
            locals,
            option,
            proven_exhaustive_gets,
        }
    }

    #[must_use]
    pub(crate) fn contains(&self, local: LocalId) -> bool {
        self.locals.contains(&local)
    }

    pub(crate) fn locals(&self) -> impl Iterator<Item = LocalId> + '_ {
        self.locals.iter().copied()
    }

    /// Proves that a range body is exactly one direct append into compiler-
    /// private `List[Int]` storage. This proof lets the emitter carry the
    /// private header in loop SSA while every unrecognized shape keeps using
    /// the general builtin lowering.
    #[must_use]
    pub(crate) fn direct_append_loop<'a>(
        &self,
        body: &'a Block,
    ) -> Option<NativeIntListAppendLoop<'a>> {
        let append = exact_single_append_body(body)?;
        self.contains(append.local).then_some(append)
    }

    /// Recognizes the only `List.get` consumer accepted by the plan: a direct,
    /// exhaustive `Option[Int]` match. The analyzer has already rejected any
    /// receiver mutation nested in the index expression.
    #[must_use]
    pub(crate) fn direct_get_match<'a>(
        &self,
        active_range: Option<LocalId>,
        scrutinee: &'a Expr,
        arms: &'a [MatchArm],
    ) -> Option<NativeIntListGetMatch<'a>> {
        let (local, index) = int_list_get_call(scrutinee)?;
        if !self.contains(local) {
            return None;
        }
        option_match(self.option?, arms).map(|arms| {
            let index_proven_in_bounds = active_range.is_some_and(|range| {
                self.proven_exhaustive_gets.contains(&(range, local))
                    && is_exact_local_copy(index, range)
            });
            NativeIntListGetMatch {
                local,
                index,
                some: arms.some,
                none: arms.none,
                some_binding: arms.some_binding,
                index_proven_in_bounds,
            }
        })
    }

    #[cfg(test)]
    fn has_proven_exhaustive_get(&self, range: LocalId, list: LocalId) -> bool {
        self.proven_exhaustive_gets.contains(&(range, list))
    }
}

#[derive(Clone, Copy)]
pub(crate) struct NativeIntListAppendLoop<'a> {
    pub(crate) local: LocalId,
    pub(crate) value: &'a Expr,
    pub(crate) value_observes_receiver: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct NativeIntListGetMatch<'a> {
    pub(crate) local: LocalId,
    pub(crate) index: &'a Expr,
    pub(crate) some: &'a MatchArm,
    pub(crate) none: &'a MatchArm,
    pub(crate) some_binding: Option<LocalId>,
    pub(crate) index_proven_in_bounds: bool,
}

struct OptionArms<'a> {
    some: &'a MatchArm,
    none: &'a MatchArm,
    some_binding: Option<LocalId>,
}

fn option_match(option: TypeId, arms: &[MatchArm]) -> Option<OptionArms<'_>> {
    if arms.len() != 2 {
        return None;
    }
    let mut some = None;
    let mut none = None;
    let mut some_binding = None;
    for arm in arms {
        let Pattern::Variant {
            ty,
            variant,
            payload,
        } = &arm.pattern
        else {
            return None;
        };
        if *ty != option {
            return None;
        }
        match variant.0 {
            0 if payload.is_empty() && arm.bindings.is_empty() && none.is_none() => {
                none = Some(arm);
            }
            1 if payload.len() == 1 && some.is_none() => match payload.as_slice() {
                [Pattern::Binding] if arm.bindings.len() == 1 => {
                    some_binding = arm.bindings.first().copied();
                    some = Some(arm);
                }
                [Pattern::Wildcard] if arm.bindings.is_empty() => {
                    some = Some(arm);
                }
                _ => return None,
            },
            _ => return None,
        }
    }
    Some(OptionArms {
        some: some?,
        none: none?,
        some_binding,
    })
}

/// A value which is guaranteed not to change between two structured range
/// statements. Keeping this key deliberately small makes equality of the two
/// captured range ends a proof, rather than an optimistic expression match.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum StableIntValue {
    Constant(i64),
    ImmutableLocal(LocalId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntListLengthFact {
    Unknown,
    Empty,
    /// The list contains exactly `max(end, 0)` elements after a completed
    /// zero-based range with one successful append per normal iteration.
    ExactZeroBasedRange(StableIntValue),
}

fn prove_exhaustive_int_list_gets(
    function: &Function,
    option: TypeId,
    list: LocalId,
    range_binding_counts: &BTreeMap<LocalId, usize>,
) -> BTreeSet<(LocalId, LocalId)> {
    let mut proven = BTreeSet::new();
    let mut length = IntListLengthFact::Unknown;
    for statement in &function.body.statements {
        match &statement.kind {
            StatementKind::Let { local, value } if *local == list => {
                length = if is_empty_int_list(value) {
                    IntListLengthFact::Empty
                } else {
                    IntListLengthFact::Unknown
                };
            }
            StatementKind::ForRange {
                local,
                start,
                end,
                body,
            } => {
                if expr_mutates_list(start, list) || expr_mutates_list(end, list) {
                    length = IntListLengthFact::Unknown;
                }
                let range_end = zero_based_stable_range_end(function, start, end);
                if matches!(
                    (length, range_end),
                    (
                        IntListLengthFact::ExactZeroBasedRange(proven_end),
                        Some(scan_end)
                    ) if proven_end == scan_end
                ) && range_binding_counts.get(local) == Some(&1)
                    && exact_exhaustive_get_body(body, option, list, *local)
                {
                    proven.insert((*local, list));
                }

                if block_mutates_list(body, list) {
                    length = if length == IntListLengthFact::Empty
                        && exact_single_append_body(body).is_some_and(|append| append.local == list)
                    {
                        range_end.map_or(IntListLengthFact::Unknown, |end| {
                            IntListLengthFact::ExactZeroBasedRange(end)
                        })
                    } else {
                        IntListLengthFact::Unknown
                    };
                }
            }
            _ if statement_mutates_list(&statement.kind, list) => {
                length = IntListLengthFact::Unknown;
            }
            _ => {}
        }
    }
    proven
}

fn zero_based_stable_range_end(
    function: &Function,
    start: &Expr,
    end: &Expr,
) -> Option<StableIntValue> {
    matches!(start.kind, ExprKind::Constant(Constant::Int(0)))
        .then(|| stable_int_value(function, end))
        .flatten()
}

fn stable_int_value(function: &Function, expression: &Expr) -> Option<StableIntValue> {
    match &expression.kind {
        ExprKind::Constant(Constant::Int(value)) => Some(StableIntValue::Constant(*value)),
        ExprKind::Copy(place) if place.projection.is_empty() => function
            .params
            .iter()
            .chain(&function.locals)
            .find(|local| local.id == place.local && local.ty == Type::Int && !local.mutable)
            .map(|_| StableIntValue::ImmutableLocal(place.local)),
        _ => None,
    }
}

/// Recognizes the lowering's canonical append loop body. Calls nested in the
/// element expression may fault, but a path which reaches the later scan has
/// completed exactly one append for every normal range iteration.
fn exact_single_append_body(body: &Block) -> Option<NativeIntListAppendLoop<'_>> {
    let [statement] = body.statements.as_slice() else {
        return None;
    };
    let StatementKind::Evaluate(Expr {
        kind:
            ExprKind::Call {
                target: CallTarget::Builtin(Builtin::ListAdd),
                type_arguments,
                arguments,
                witnesses,
            },
        ..
    }) = &statement.kind
    else {
        return None;
    };
    let [CallArgument::InOut(receiver), CallArgument::Value(value)] = arguments.as_slice() else {
        return None;
    };
    let local = receiver.local;
    (type_arguments.is_empty()
        && witnesses.is_empty()
        && receiver.projection.is_empty()
        && !expr_mutates_list(value, local)
        && block_has_unit_tail(body))
    .then_some(NativeIntListAppendLoop {
        local,
        value,
        value_observes_receiver: expr_references_local(value, local),
    })
}

/// Recognizes a direct exhaustive `Option[Int]` consumer for the induction
/// variable. The entire loop body must leave the private list unchanged, so
/// the exact-length fact remains true for every iteration.
fn exact_exhaustive_get_body(
    body: &Block,
    option: TypeId,
    list: LocalId,
    induction: LocalId,
) -> bool {
    if block_mutates_list(body, list) || !block_has_unit_tail(body) {
        return false;
    }
    let [statement] = body.statements.as_slice() else {
        return false;
    };
    let StatementKind::Evaluate(Expr {
        kind: ExprKind::Match { scrutinee, arms },
        ..
    }) = &statement.kind
    else {
        return false;
    };
    let Some((receiver, index)) = int_list_get_call(scrutinee) else {
        return false;
    };
    receiver == list
        && is_exact_local_copy(index, induction)
        && option_match(option, arms).is_some()
}

fn block_has_unit_tail(block: &Block) -> bool {
    block.tail.as_deref().is_some_and(|tail| {
        tail.ty == Type::Unit && matches!(tail.kind, ExprKind::Constant(Constant::Unit))
    })
}

fn is_exact_local_copy(expression: &Expr, local: LocalId) -> bool {
    matches!(&expression.kind, ExprKind::Copy(place) if exact_local(place, local))
}

fn statement_mutates_list(statement: &StatementKind, list: LocalId) -> bool {
    match statement {
        StatementKind::Let { local, value } => *local == list || expr_mutates_list(value, list),
        StatementKind::Scoped { local, value, .. } => {
            *local == list || expr_mutates_list(value, list)
        }
        StatementKind::LetTuple { locals, value } => {
            locals.contains(&list) || expr_mutates_list(value, list)
        }
        StatementKind::ForRange {
            start, end, body, ..
        } => {
            expr_mutates_list(start, list)
                || expr_mutates_list(end, list)
                || block_mutates_list(body, list)
        }
        StatementKind::Assign { place, value } => {
            place.local == list || expr_mutates_list(value, list)
        }
        StatementKind::Assert { condition } | StatementKind::Evaluate(condition) => {
            expr_mutates_list(condition, list)
        }
        StatementKind::Defer(cleanup) => block_mutates_list(cleanup, list),
        StatementKind::Return(value) => value
            .as_ref()
            .is_some_and(|value| expr_mutates_list(value, list)),
    }
}

fn block_mutates_list(block: &Block, list: LocalId) -> bool {
    block
        .statements
        .iter()
        .any(|statement| statement_mutates_list(&statement.kind, list))
        || block
            .tail
            .as_deref()
            .is_some_and(|tail| expr_mutates_list(tail, list))
}

#[allow(clippy::too_many_lines)]
fn expr_mutates_list(expression: &Expr, list: LocalId) -> bool {
    match &expression.kind {
        ExprKind::Constant(_) | ExprKind::Copy(_) => false,
        ExprKind::Move(place) => place.local == list,
        ExprKind::Tuple(values)
        | ExprKind::List(values)
        | ExprKind::TaskJoin {
            arguments: values, ..
        } => values.iter().any(|value| expr_mutates_list(value, list)),
        ExprKind::Unary(_, value)
        | ExprKind::Unrefine(value)
        | ExprKind::Refine { value, .. }
        | ExprKind::Await { task: value, .. }
        | ExprKind::Sleep {
            milliseconds: value,
        } => expr_mutates_list(value, list),
        ExprKind::Binary(_, left, right) => {
            expr_mutates_list(left, list) || expr_mutates_list(right, list)
        }
        ExprKind::Block(block) => block_mutates_list(block, list),
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            expr_mutates_list(condition, list)
                || block_mutates_list(then_branch, list)
                || block_mutates_list(else_branch, list)
        }
        ExprKind::Match { scrutinee, arms } => {
            expr_mutates_list(scrutinee, list)
                || arms.iter().any(|arm| expr_mutates_list(&arm.value, list))
        }
        ExprKind::Record { fields, .. } => {
            fields.iter().any(|field| expr_mutates_list(field, list))
        }
        ExprKind::Variant { payload, .. } => {
            payload.iter().any(|value| expr_mutates_list(value, list))
        }
        ExprKind::Call { arguments, .. } => arguments.iter().any(|argument| match argument {
            CallArgument::Value(value) => expr_mutates_list(value, list),
            CallArgument::InOut(place) => place.local == list,
        }),
        ExprKind::MakeView {
            value, writeback, ..
        } => {
            expr_mutates_list(value, list)
                || writeback.as_ref().is_some_and(|place| place.local == list)
        }
        ExprKind::ReborrowView { owner, .. } => owner.local == list,
    }
}

struct IntListUseScanner {
    local: LocalId,
    option: Option<TypeId>,
    initializers: usize,
    valid: bool,
    /// Counts relevant structured range binders while this existing use
    /// scanner walks the body. Reusing the traversal keeps the bounds-proof
    /// key defensive without a second full MIR visitor.
    range_binding_counts: BTreeMap<LocalId, usize>,
}

impl IntListUseScanner {
    fn new(local: LocalId, option: Option<TypeId>) -> Self {
        Self {
            local,
            option,
            initializers: 0,
            valid: true,
            range_binding_counts: BTreeMap::new(),
        }
    }

    fn forbid(&mut self) {
        self.valid = false;
    }

    fn scan_block(&mut self, block: &Block, allow_initializer: bool) {
        for statement in &block.statements {
            match &statement.kind {
                StatementKind::Let { local, value } => {
                    if *local == self.local {
                        if !allow_initializer || !is_empty_int_list(value) {
                            self.forbid();
                        } else {
                            self.initializers = self.initializers.saturating_add(1);
                        }
                    } else {
                        self.scan_expr(value);
                    }
                }
                StatementKind::Scoped { local, value, .. } => {
                    if *local == self.local {
                        self.forbid();
                    }
                    self.scan_expr(value);
                }
                StatementKind::LetTuple { locals, value } => {
                    if locals.contains(&self.local) {
                        self.forbid();
                    }
                    self.scan_expr(value);
                }
                StatementKind::ForRange {
                    local,
                    start,
                    end,
                    body,
                } => {
                    *self.range_binding_counts.entry(*local).or_default() += 1;
                    if *local == self.local {
                        self.forbid();
                    }
                    self.scan_expr(start);
                    self.scan_expr(end);
                    self.scan_block(body, false);
                }
                StatementKind::Assign { place, value } => {
                    if place.local == self.local {
                        self.forbid();
                    }
                    self.scan_expr(value);
                }
                StatementKind::Assert { condition } | StatementKind::Evaluate(condition) => {
                    self.scan_expr(condition);
                }
                StatementKind::Defer(cleanup) => {
                    if block_references_local(cleanup, self.local) {
                        self.forbid();
                    }
                }
                StatementKind::Return(value) => {
                    if let Some(value) = value {
                        self.scan_expr(value);
                    }
                }
            }
        }
        if let Some(tail) = &block.tail {
            self.scan_expr(tail);
        }
    }

    #[allow(clippy::too_many_lines)]
    fn scan_expr(&mut self, expression: &Expr) {
        match &expression.kind {
            ExprKind::Constant(_) => {}
            ExprKind::Copy(place) | ExprKind::Move(place) => self.scan_forbidden_place(place),
            ExprKind::Tuple(values)
            | ExprKind::List(values)
            | ExprKind::TaskJoin {
                arguments: values, ..
            } => {
                for value in values {
                    self.scan_expr(value);
                }
            }
            ExprKind::Unary(_, value)
            | ExprKind::Unrefine(value)
            | ExprKind::Refine { value, .. }
            | ExprKind::Sleep {
                milliseconds: value,
            } => self.scan_expr(value),
            ExprKind::Await { task, .. } => {
                if expr_references_local(task, self.local) {
                    self.forbid();
                }
                self.scan_expr(task);
            }
            ExprKind::Binary(_, left, right) => {
                self.scan_expr(left);
                self.scan_expr(right);
            }
            ExprKind::Block(block) => self.scan_block(block, false),
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.scan_expr(condition);
                self.scan_block(then_branch, false);
                self.scan_block(else_branch, false);
            }
            ExprKind::Match { scrutinee, arms } => {
                if let Some((local, index)) = int_list_get_call(scrutinee)
                    && local == self.local
                    && self
                        .option
                        .and_then(|option| option_match(option, arms))
                        .is_some()
                {
                    if expr_references_local(index, self.local) {
                        // Appending while evaluating the index may reallocate
                        // and invalidate a pre-evaluation data snapshot.
                        self.forbid();
                    }
                    self.scan_expr(index);
                } else {
                    self.scan_expr(scrutinee);
                }
                for arm in arms {
                    self.scan_expr(&arm.value);
                }
            }
            ExprKind::Record { fields, .. } => {
                for field in fields {
                    self.scan_expr(field);
                }
            }
            ExprKind::Variant { payload, .. } => {
                for value in payload {
                    self.scan_expr(value);
                }
            }
            ExprKind::Call {
                target,
                type_arguments,
                arguments,
                witnesses,
            } => {
                if !type_arguments.is_empty() || !witnesses.is_empty() {
                    self.scan_call_arguments_forbidden(arguments);
                    return;
                }
                match target {
                    CallTarget::Builtin(Builtin::ListAdd)
                        if matches!(
                            arguments.as_slice(),
                            [CallArgument::InOut(place), CallArgument::Value(_)]
                                if exact_local(place, self.local)
                        ) =>
                    {
                        let CallArgument::Value(value) = &arguments[1] else {
                            unreachable!("shape checked above")
                        };
                        self.scan_expr(value);
                    }
                    CallTarget::Builtin(Builtin::ListLength)
                        if matches!(
                            arguments.as_slice(),
                            [CallArgument::Value(Expr { kind: ExprKind::Copy(place), .. })]
                                if exact_local(place, self.local)
                        ) => {}
                    _ => self.scan_call_arguments_forbidden(arguments),
                }
            }
            ExprKind::MakeView {
                value, writeback, ..
            } => {
                if expr_references_local(value, self.local)
                    || writeback
                        .as_ref()
                        .is_some_and(|place| place.local == self.local)
                {
                    self.forbid();
                }
                self.scan_expr(value);
            }
            ExprKind::ReborrowView { owner, .. } => self.scan_forbidden_place(owner),
        }
    }

    fn scan_call_arguments_forbidden(&mut self, arguments: &[CallArgument]) {
        for argument in arguments {
            match argument {
                CallArgument::Value(value) => self.scan_expr(value),
                CallArgument::InOut(place) => self.scan_forbidden_place(place),
            }
        }
    }

    fn scan_forbidden_place(&mut self, place: &Place) {
        if place.local == self.local {
            self.forbid();
        }
    }
}

fn exact_local(place: &Place, local: LocalId) -> bool {
    place.local == local && place.projection.is_empty()
}

fn is_empty_int_list(expression: &Expr) -> bool {
    expression.ty == Type::List(Box::new(Type::Int))
        && matches!(&expression.kind, ExprKind::List(values) if values.is_empty())
}

fn int_list_get_call(expression: &Expr) -> Option<(LocalId, &Expr)> {
    let ExprKind::Call {
        target: CallTarget::Builtin(Builtin::ListGet),
        type_arguments,
        arguments,
        witnesses,
    } = &expression.kind
    else {
        return None;
    };
    if !type_arguments.is_empty() || !witnesses.is_empty() {
        return None;
    }
    let [CallArgument::Value(receiver), CallArgument::Value(index)] = arguments.as_slice() else {
        return None;
    };
    let ExprKind::Copy(place) = &receiver.kind else {
        return None;
    };
    place.projection.is_empty().then_some((place.local, index))
}

fn block_references_local(block: &Block, local: LocalId) -> bool {
    block
        .statements
        .iter()
        .any(|statement| match &statement.kind {
            StatementKind::Scoped {
                local: destination,
                value,
                ..
            }
            | StatementKind::Let {
                local: destination,
                value,
            } => *destination == local || expr_references_local(value, local),
            StatementKind::LetTuple { locals, value } => {
                locals.contains(&local) || expr_references_local(value, local)
            }
            StatementKind::ForRange {
                local: iteration,
                start,
                end,
                body,
            } => {
                *iteration == local
                    || expr_references_local(start, local)
                    || expr_references_local(end, local)
                    || block_references_local(body, local)
            }
            StatementKind::Assign { place, value } => {
                place.local == local || expr_references_local(value, local)
            }
            StatementKind::Assert { condition } | StatementKind::Evaluate(condition) => {
                expr_references_local(condition, local)
            }
            StatementKind::Defer(cleanup) => block_references_local(cleanup, local),
            StatementKind::Return(value) => value
                .as_ref()
                .is_some_and(|value| expr_references_local(value, local)),
        })
        || block
            .tail
            .as_deref()
            .is_some_and(|tail| expr_references_local(tail, local))
}

#[allow(clippy::too_many_lines)]
fn expr_references_local(expression: &Expr, local: LocalId) -> bool {
    match &expression.kind {
        ExprKind::Constant(_) => false,
        ExprKind::Copy(place) | ExprKind::Move(place) => place.local == local,
        ExprKind::Tuple(values)
        | ExprKind::List(values)
        | ExprKind::TaskJoin {
            arguments: values, ..
        } => values
            .iter()
            .any(|value| expr_references_local(value, local)),
        ExprKind::Unary(_, value)
        | ExprKind::Unrefine(value)
        | ExprKind::Refine { value, .. }
        | ExprKind::Await { task: value, .. }
        | ExprKind::Sleep {
            milliseconds: value,
        } => expr_references_local(value, local),
        ExprKind::Binary(_, left, right) => {
            expr_references_local(left, local) || expr_references_local(right, local)
        }
        ExprKind::Block(block) => block_references_local(block, local),
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            expr_references_local(condition, local)
                || block_references_local(then_branch, local)
                || block_references_local(else_branch, local)
        }
        ExprKind::Match { scrutinee, arms } => {
            expr_references_local(scrutinee, local)
                || arms
                    .iter()
                    .any(|arm| expr_references_local(&arm.value, local))
        }
        ExprKind::Record { fields, .. } => fields
            .iter()
            .any(|field| expr_references_local(field, local)),
        ExprKind::Variant { payload, .. } => payload
            .iter()
            .any(|value| expr_references_local(value, local)),
        ExprKind::Call { arguments, .. } => arguments.iter().any(|argument| match argument {
            CallArgument::Value(value) => expr_references_local(value, local),
            CallArgument::InOut(place) => place.local == local,
        }),
        ExprKind::MakeView {
            value, writeback, ..
        } => {
            expr_references_local(value, local)
                || writeback.as_ref().is_some_and(|place| place.local == local)
        }
        ExprKind::ReborrowView { owner, .. } => owner.local == local,
    }
}

#[cfg(test)]
#[allow(clippy::default_trait_access)]
mod tests {
    use super::{NativeIntListPlan, native_pod_value_argument_local, option_match};
    use loom_mir::{
        Block, Builtin, CallArgument, CallPlan, CallTarget, Constant, Expr, ExprKind, Function,
        FunctionId, LocalDecl, LocalId, MatchArm, Pattern, Place, PreludeIds, Program, Statement,
        StatementKind, Type, TypeId, VariantId,
    };

    const LIST: LocalId = LocalId(0);
    const VALUE: LocalId = LocalId(1);
    const BUILD_RANGE: LocalId = LocalId(2);
    const SCAN_RANGE: LocalId = LocalId(3);
    const OPTION: TypeId = TypeId(7);

    fn expression(kind: ExprKind, ty: Type) -> Expr {
        Expr::new(kind, ty, Default::default())
    }

    fn function(statements: Vec<Statement>) -> Function {
        Function {
            id: FunctionId(0),
            name: "shape".into(),
            span: Default::default(),
            type_parameters: 0,
            is_async: false,
            suspension_points: Vec::new(),
            params: Vec::new(),
            witness_params: Vec::new(),
            witness_prefix_count: 0,
            locals: vec![
                LocalDecl {
                    id: LIST,
                    name: "values".into(),
                    ty: Type::List(Box::new(Type::Int)),
                    mutable: true,
                    span: Default::default(),
                },
                LocalDecl {
                    id: VALUE,
                    name: "value".into(),
                    ty: Type::Int,
                    mutable: false,
                    span: Default::default(),
                },
                LocalDecl {
                    id: BUILD_RANGE,
                    name: "build_index".into(),
                    ty: Type::Int,
                    mutable: false,
                    span: Default::default(),
                },
                LocalDecl {
                    id: SCAN_RANGE,
                    name: "scan_index".into(),
                    ty: Type::Int,
                    mutable: false,
                    span: Default::default(),
                },
            ],
            return_ty: Type::Unit,
            receiver: None,
            body: Block {
                statements,
                tail: Some(Box::new(expression(
                    ExprKind::Constant(loom_mir::Constant::Unit),
                    Type::Unit,
                ))),
                span: Default::default(),
            },
            call_plan: CallPlan::default(),
        }
    }

    fn program() -> Program {
        Program {
            prelude: PreludeIds {
                option: Some(OPTION),
                ..PreludeIds::default()
            },
            ..Program::default()
        }
    }

    fn initialize() -> Statement {
        Statement {
            kind: StatementKind::Let {
                local: LIST,
                value: expression(ExprKind::List(Vec::new()), Type::List(Box::new(Type::Int))),
            },
            span: Default::default(),
        }
    }

    fn unit() -> Expr {
        expression(ExprKind::Constant(Constant::Unit), Type::Unit)
    }

    fn integer(value: i64) -> Expr {
        expression(ExprKind::Constant(Constant::Int(value)), Type::Int)
    }

    fn copy(local: LocalId) -> Expr {
        expression(ExprKind::Copy(Place::local(local)), Type::Int)
    }

    fn block(statements: Vec<Statement>) -> Block {
        Block {
            statements,
            tail: Some(Box::new(unit())),
            span: Default::default(),
        }
    }

    fn append(value: Expr) -> Statement {
        Statement {
            kind: StatementKind::Evaluate(expression(
                ExprKind::Call {
                    target: CallTarget::Builtin(Builtin::ListAdd),
                    type_arguments: Vec::new(),
                    arguments: vec![
                        CallArgument::InOut(Place::local(LIST)),
                        CallArgument::Value(value),
                    ],
                    witnesses: Vec::new(),
                },
                Type::Unit,
            )),
            span: Default::default(),
        }
    }

    fn option_arms() -> Vec<MatchArm> {
        vec![
            MatchArm {
                pattern: Pattern::Variant {
                    ty: OPTION,
                    variant: VariantId(1),
                    payload: vec![Pattern::Binding],
                },
                bindings: vec![VALUE],
                value: unit(),
            },
            MatchArm {
                pattern: Pattern::Variant {
                    ty: OPTION,
                    variant: VariantId(0),
                    payload: Vec::new(),
                },
                bindings: Vec::new(),
                value: unit(),
            },
        ]
    }

    fn direct_get_match(index: LocalId) -> Statement {
        direct_get_match_with_index(copy(index))
    }

    fn direct_get_match_with_index(index: Expr) -> Statement {
        let get = expression(
            ExprKind::Call {
                target: CallTarget::Builtin(Builtin::ListGet),
                type_arguments: Vec::new(),
                arguments: vec![
                    CallArgument::Value(expression(
                        ExprKind::Copy(Place::local(LIST)),
                        Type::List(Box::new(Type::Int)),
                    )),
                    CallArgument::Value(index),
                ],
                witnesses: Vec::new(),
            },
            Type::Nominal(OPTION, vec![Type::Int]),
        );
        Statement {
            kind: StatementKind::Evaluate(expression(
                ExprKind::Match {
                    scrutinee: Box::new(get),
                    arms: option_arms(),
                },
                Type::Unit,
            )),
            span: Default::default(),
        }
    }

    #[test]
    fn pod_value_argument_source_requires_a_flat_local_copy() {
        let flat = CallArgument::Value(expression(
            ExprKind::Copy(Place::local(VALUE)),
            Type::Nominal(TypeId(8), Vec::new()),
        ));
        assert_eq!(native_pod_value_argument_local(&flat), Some(VALUE));

        let projected = CallArgument::Value(expression(
            ExprKind::Copy(Place {
                local: VALUE,
                projection: vec![0],
            }),
            Type::Int,
        ));
        assert_eq!(native_pod_value_argument_local(&projected), None);
        assert_eq!(
            native_pod_value_argument_local(&CallArgument::InOut(Place::local(VALUE))),
            None
        );
        assert_eq!(
            native_pod_value_argument_local(&CallArgument::Value(integer(1))),
            None
        );
    }

    fn range(local: LocalId, end: i64, body: Block) -> Statement {
        range_from(local, 0, end, body)
    }

    fn range_from(local: LocalId, start: i64, end: i64, body: Block) -> Statement {
        Statement {
            kind: StatementKind::ForRange {
                local,
                start: Box::new(integer(start)),
                end: Box::new(integer(end)),
                body: Box::new(body),
            },
            span: Default::default(),
        }
    }

    fn exact_build_and_scan() -> Function {
        function(vec![
            initialize(),
            range(BUILD_RANGE, 4, block(vec![append(copy(BUILD_RANGE))])),
            range(SCAN_RANGE, 4, block(vec![direct_get_match(SCAN_RANGE)])),
        ])
    }

    #[test]
    fn empty_unused_int_list_is_native_but_copy_escape_is_not() {
        let native = function(vec![initialize()]);
        assert!(NativeIntListPlan::analyze(&program(), &native).contains(LIST));

        let mut escaped = function(vec![initialize()]);
        escaped.body.statements.push(Statement {
            kind: StatementKind::Evaluate(expression(
                ExprKind::Copy(loom_mir::Place::local(LIST)),
                Type::List(Box::new(Type::Int)),
            )),
            span: Default::default(),
        });
        assert!(!NativeIntListPlan::analyze(&program(), &escaped).contains(LIST));
    }

    #[test]
    fn option_shape_requires_explicit_exhaustive_none_and_some() {
        let some = MatchArm {
            pattern: Pattern::Variant {
                ty: OPTION,
                variant: VariantId(1),
                payload: vec![Pattern::Binding],
            },
            bindings: vec![VALUE],
            value: expression(ExprKind::Constant(loom_mir::Constant::Unit), Type::Unit),
        };
        let none = MatchArm {
            pattern: Pattern::Variant {
                ty: OPTION,
                variant: VariantId(0),
                payload: Vec::new(),
            },
            bindings: Vec::new(),
            value: expression(ExprKind::Constant(loom_mir::Constant::Unit), Type::Unit),
        };
        let arms = [some, none];
        let matched = option_match(OPTION, &arms).expect("exhaustive Option match");
        assert_eq!(matched.some_binding, Some(VALUE));

        assert!(option_match(OPTION, &arms[..1]).is_none());
        assert!(option_match(TypeId(9), &arms).is_none());
    }

    #[test]
    fn exact_completed_append_range_proves_exhaustive_get_range() {
        let function = exact_build_and_scan();
        let plan = NativeIntListPlan::analyze(&program(), &function);
        assert!(plan.contains(LIST));
        assert!(plan.has_proven_exhaustive_get(SCAN_RANGE, LIST));

        let StatementKind::ForRange { body, .. } = &function.body.statements[1].kind else {
            panic!("build statement is not a range")
        };
        let append = plan
            .direct_append_loop(body)
            .expect("canonical direct append loop");
        assert_eq!(append.local, LIST);
        assert!(!append.value_observes_receiver);
        assert!(matches!(
            append.value.kind,
            ExprKind::Copy(ref place) if place.local == BUILD_RANGE
        ));
    }

    #[test]
    fn append_loop_marks_receiver_observation() {
        let list = expression(
            ExprKind::Copy(Place::local(LIST)),
            Type::List(Box::new(Type::Int)),
        );
        let length = expression(
            ExprKind::Call {
                target: CallTarget::Builtin(Builtin::ListLength),
                type_arguments: Vec::new(),
                arguments: vec![CallArgument::Value(list)],
                witnesses: Vec::new(),
            },
            Type::Int,
        );
        let nested_length = expression(
            ExprKind::Block(Block {
                statements: Vec::new(),
                tail: Some(Box::new(length)),
                span: Default::default(),
            }),
            Type::Int,
        );
        let function = function(vec![
            initialize(),
            range(BUILD_RANGE, 4, block(vec![append(nested_length)])),
        ]);
        let plan = NativeIntListPlan::analyze(&program(), &function);
        assert!(plan.contains(LIST));
        let StatementKind::ForRange { body, .. } = &function.body.statements[1].kind else {
            panic!("build statement is not a range")
        };
        let append = plan
            .direct_append_loop(body)
            .expect("receiver-observing append remains a specialized loop");
        assert!(append.value_observes_receiver);
    }

    #[test]
    fn append_loop_proof_rejects_multiple_mutations() {
        let function = function(vec![
            initialize(),
            range(
                BUILD_RANGE,
                4,
                block(vec![append(copy(BUILD_RANGE)), append(integer(99))]),
            ),
        ]);
        let plan = NativeIntListPlan::analyze(&program(), &function);
        assert!(plan.contains(LIST));
        let StatementKind::ForRange { body, .. } = &function.body.statements[1].kind else {
            panic!("build statement is not a range")
        };
        assert!(plan.direct_append_loop(body).is_none());
    }

    #[test]
    fn intervening_append_and_reused_range_binding_prevent_bounds_proof() {
        let mut appended = exact_build_and_scan();
        appended.body.statements.insert(2, append(integer(99)));
        let plan = NativeIntListPlan::analyze(&program(), &appended);
        assert!(plan.contains(LIST));
        assert!(!plan.has_proven_exhaustive_get(SCAN_RANGE, LIST));

        let mut reused_binding = exact_build_and_scan();
        reused_binding
            .body
            .statements
            .insert(2, range(SCAN_RANGE, 0, block(Vec::new())));
        let plan = NativeIntListPlan::analyze(&program(), &reused_binding);
        assert!(plan.contains(LIST));
        assert!(!plan.has_proven_exhaustive_get(SCAN_RANGE, LIST));
    }

    #[test]
    fn non_exact_append_and_scan_shapes_keep_checked_get_semantics() {
        let cases = [
            (
                "different range ends",
                function(vec![
                    initialize(),
                    range(BUILD_RANGE, 4, block(vec![append(copy(BUILD_RANGE))])),
                    range(SCAN_RANGE, 5, block(vec![direct_get_match(SCAN_RANGE)])),
                ]),
            ),
            (
                "nonzero build start",
                function(vec![
                    initialize(),
                    range_from(BUILD_RANGE, 1, 4, block(vec![append(copy(BUILD_RANGE))])),
                    range(SCAN_RANGE, 4, block(vec![direct_get_match(SCAN_RANGE)])),
                ]),
            ),
            (
                "two appends per iteration",
                function(vec![
                    initialize(),
                    range(
                        BUILD_RANGE,
                        4,
                        block(vec![append(copy(BUILD_RANGE)), append(integer(99))]),
                    ),
                    range(SCAN_RANGE, 4, block(vec![direct_get_match(SCAN_RANGE)])),
                ]),
            ),
            (
                "non-induction index",
                function(vec![
                    initialize(),
                    range(BUILD_RANGE, 4, block(vec![append(copy(BUILD_RANGE))])),
                    range(
                        SCAN_RANGE,
                        4,
                        block(vec![direct_get_match_with_index(integer(0))]),
                    ),
                ]),
            ),
            (
                "scan mutates list",
                function(vec![
                    initialize(),
                    range(BUILD_RANGE, 4, block(vec![append(copy(BUILD_RANGE))])),
                    range(
                        SCAN_RANGE,
                        4,
                        block(vec![append(integer(99)), direct_get_match(SCAN_RANGE)]),
                    ),
                ]),
            ),
        ];

        for (label, function) in cases {
            let plan = NativeIntListPlan::analyze(&program(), &function);
            assert!(plan.contains(LIST), "{label}");
            assert!(
                !plan.has_proven_exhaustive_get(SCAN_RANGE, LIST),
                "unexpected bounds proof for {label}"
            );
        }
    }
}
