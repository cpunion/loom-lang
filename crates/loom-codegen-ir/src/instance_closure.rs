use std::collections::BTreeMap;

use loom_core::Span;
use loom_mir::{
    self as mir, CallArgument, CallTarget, ExprId, ExprKind, FunctionId, StatementKind, Type,
    WitnessRef,
};

use crate::{INSTANCE_KEY_STRUCTURE_BUDGET, InstanceKey, InstanceWitnessArgument};

/// Maximum number of concrete callable instances in one LCIR artifact.
///
/// This is an implementation resource bound rather than a source-language
/// promise. It is deliberately checked while planning, before any LCIR table
/// or instantiated MIR type is allocated.
pub const INSTANCE_CLOSURE_MAX_INSTANCES: usize = 4_096;

/// Maximum number of reachable direct-call edges in one LCIR artifact.
pub const INSTANCE_CLOSURE_MAX_CALL_EDGES: usize = 16_384;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InstanceClosureUnsupportedKind {
    InstanceBudget,
    NonRegularRecursion,
    Instantiation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InstanceClosureUnsupported {
    pub(crate) kind: InstanceClosureUnsupportedKind,
    pub(crate) function: FunctionId,
    pub(crate) expression: Option<ExprId>,
    pub(crate) span: Span,
    pub(crate) path: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum InstanceClosureError {
    MissingFunction(FunctionId),
    InvalidInstanceArity {
        function: FunctionId,
        expected_types: usize,
        actual_types: usize,
        expected_witnesses: usize,
        actual_witnesses: usize,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct InstanceClosure {
    entries: Vec<InstanceKey>,
    calls: BTreeMap<String, Box<[InstanceKey]>>,
}

impl InstanceClosure {
    pub(crate) fn entries(&self) -> &[InstanceKey] {
        &self.entries
    }

    pub(crate) fn calls(&self, caller: &InstanceKey) -> Option<&[InstanceKey]> {
        self.calls
            .get(&caller.canonical_identity())
            .map(Box::as_ref)
    }
}

pub(crate) enum InstanceClosureOutcome {
    Complete(InstanceClosure),
    Unsupported(InstanceClosureUnsupported),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InstantiationError {
    StructureBudget,
    UnboundTypeParameter,
    UnboundWitnessParameter,
    UnresolvedAssociatedProjection,
}

/// A bounded view of one concrete source-function instance.
///
/// Every public operation performs a node-count preflight before cloning any
/// substituted type or witness tree. This keeps polymorphic expansion an
/// atomic route-selection concern instead of a late allocator failure.
pub(crate) struct InstanceSubstitution<'key> {
    key: &'key InstanceKey,
}

impl<'key> InstanceSubstitution<'key> {
    pub(crate) const fn new(key: &'key InstanceKey) -> Self {
        Self { key }
    }

    pub(crate) fn instantiate_type(&self, ty: &Type) -> Result<Type, InstantiationError> {
        self.preflight_types(std::slice::from_ref(ty))?;
        self.clone_type(ty, true)
    }

    pub(crate) fn instantiate_types(
        &self,
        types: &[Type],
    ) -> Result<Vec<Type>, InstantiationError> {
        self.preflight_types(types)?;
        types.iter().map(|ty| self.clone_type(ty, true)).collect()
    }

    pub(crate) fn call_key(
        &self,
        callee: FunctionId,
        type_arguments: &[Type],
        witnesses: &[WitnessRef],
    ) -> Result<InstanceKey, InstantiationError> {
        let types = self.instantiate_types(type_arguments)?;
        if types.iter().any(type_has_open_component) {
            return Err(InstantiationError::UnboundTypeParameter);
        }
        self.preflight_witnesses(witnesses)?;
        let witnesses = witnesses
            .iter()
            .map(|witness| self.clone_witness(witness))
            .collect::<Result<Vec<_>, _>>()?;
        let key = InstanceKey::new(callee, types, witnesses);
        key.validate_structure()
            .map_err(|_| InstantiationError::StructureBudget)?;
        Ok(key)
    }

    fn preflight_types(&self, roots: &[Type]) -> Result<(), InstantiationError> {
        enum Node<'a> {
            Source(&'a Type),
            Concrete(&'a Type),
        }

        let mut scheduled = roots.len();
        if scheduled > INSTANCE_KEY_STRUCTURE_BUDGET {
            return Err(InstantiationError::StructureBudget);
        }
        let mut work = roots.iter().rev().map(Node::Source).collect::<Vec<_>>();
        while let Some(node) = work.pop() {
            let (ty, substitute) = match node {
                Node::Source(ty) => (ty, true),
                Node::Concrete(ty) => (ty, false),
            };
            match ty {
                Type::Parameter(index) if substitute => {
                    let argument = self
                        .key
                        .type_arguments()
                        .get(*index as usize)
                        .ok_or(InstantiationError::UnboundTypeParameter)?;
                    // The concrete root replaces the already-counted
                    // parameter node; only its children add output nodes.
                    work.push(Node::Concrete(argument));
                }
                Type::Parameter(_) => return Err(InstantiationError::UnboundTypeParameter),
                Type::AssociatedProjection { .. } => {
                    return Err(InstantiationError::UnresolvedAssociatedProjection);
                }
                Type::Tuple(elements) | Type::Nominal(_, elements) => {
                    let children = elements.iter().rev().map(|child| {
                        if substitute {
                            Node::Source(child)
                        } else {
                            Node::Concrete(child)
                        }
                    });
                    schedule_nodes(&mut scheduled, elements.len(), &mut work, children)?;
                }
                Type::List(element) | Type::Task(element) | Type::TaskOutcome(element) => {
                    let child = if substitute {
                        Node::Source(element)
                    } else {
                        Node::Concrete(element)
                    };
                    schedule_nodes(&mut scheduled, 1, &mut work, std::iter::once(child))?;
                }
                Type::View { bindings, .. } => {
                    let children = bindings.values().rev().map(|child| {
                        if substitute {
                            Node::Source(child)
                        } else {
                            Node::Concrete(child)
                        }
                    });
                    schedule_nodes(&mut scheduled, bindings.len(), &mut work, children)?;
                }
                Type::Never
                | Type::Unit
                | Type::Bool
                | Type::Int
                | Type::Float
                | Type::Text
                | Type::Error => {}
            }
        }
        Ok(())
    }

    fn clone_type(&self, ty: &Type, substitute: bool) -> Result<Type, InstantiationError> {
        Ok(match ty {
            Type::Parameter(index) if substitute => self
                .key
                .type_arguments()
                .get(*index as usize)
                .cloned()
                .ok_or(InstantiationError::UnboundTypeParameter)?,
            Type::Parameter(_) => return Err(InstantiationError::UnboundTypeParameter),
            Type::AssociatedProjection { .. } => {
                return Err(InstantiationError::UnresolvedAssociatedProjection);
            }
            Type::Tuple(elements) => Type::Tuple(
                elements
                    .iter()
                    .map(|element| self.clone_type(element, substitute))
                    .collect::<Result<_, _>>()?,
            ),
            Type::List(element) => Type::List(Box::new(self.clone_type(element, substitute)?)),
            Type::Nominal(id, arguments) => Type::Nominal(
                *id,
                arguments
                    .iter()
                    .map(|argument| self.clone_type(argument, substitute))
                    .collect::<Result<_, _>>()?,
            ),
            Type::Task(output) => Type::Task(Box::new(self.clone_type(output, substitute)?)),
            Type::TaskOutcome(output) => {
                Type::TaskOutcome(Box::new(self.clone_type(output, substitute)?))
            }
            Type::View {
                mutable,
                concept,
                bindings,
            } => Type::View {
                mutable: *mutable,
                concept: *concept,
                bindings: bindings
                    .iter()
                    .map(|(name, ty)| Ok((name.clone(), self.clone_type(ty, substitute)?)))
                    .collect::<Result<_, InstantiationError>>()?,
            },
            Type::Never => Type::Never,
            Type::Unit => Type::Unit,
            Type::Bool => Type::Bool,
            Type::Int => Type::Int,
            Type::Float => Type::Float,
            Type::Text => Type::Text,
            Type::Error => Type::Error,
        })
    }

    fn preflight_witnesses(&self, roots: &[WitnessRef]) -> Result<(), InstantiationError> {
        enum Node<'a> {
            Source(&'a WitnessRef),
            Concrete(&'a InstanceWitnessArgument),
        }

        let mut scheduled = roots.len();
        if scheduled > INSTANCE_KEY_STRUCTURE_BUDGET {
            return Err(InstantiationError::StructureBudget);
        }
        let mut work = roots.iter().rev().map(Node::Source).collect::<Vec<_>>();
        while let Some(node) = work.pop() {
            match node {
                Node::Source(WitnessRef::Concrete(_)) => {}
                Node::Source(WitnessRef::Parameter(index)) => {
                    let argument = self
                        .key
                        .witness_arguments()
                        .get(*index as usize)
                        .ok_or(InstantiationError::UnboundWitnessParameter)?;
                    // The actual proof root replaces this already-counted
                    // parameter node.
                    work.push(Node::Concrete(argument));
                }
                Node::Source(WitnessRef::Apply { arguments, .. }) => {
                    schedule_nodes(
                        &mut scheduled,
                        arguments.len(),
                        &mut work,
                        arguments.iter().rev().map(Node::Source),
                    )?;
                }
                Node::Concrete(InstanceWitnessArgument::Concrete(_)) => {}
                Node::Concrete(InstanceWitnessArgument::Parameter(_)) => {
                    return Err(InstantiationError::UnboundWitnessParameter);
                }
                Node::Concrete(InstanceWitnessArgument::Apply { arguments, .. }) => {
                    schedule_nodes(
                        &mut scheduled,
                        arguments.len(),
                        &mut work,
                        arguments.iter().rev().map(Node::Concrete),
                    )?;
                }
            }
        }
        Ok(())
    }

    fn clone_witness(
        &self,
        witness: &WitnessRef,
    ) -> Result<InstanceWitnessArgument, InstantiationError> {
        match witness {
            WitnessRef::Concrete(witness) => Ok(InstanceWitnessArgument::Concrete(*witness)),
            WitnessRef::Parameter(index) => self
                .key
                .witness_arguments()
                .get(*index as usize)
                .cloned()
                .ok_or(InstantiationError::UnboundWitnessParameter),
            WitnessRef::Apply { witness, arguments } => Ok(InstanceWitnessArgument::apply(
                *witness,
                arguments
                    .iter()
                    .map(|argument| self.clone_witness(argument))
                    .collect::<Result<Vec<_>, _>>()?,
            )),
        }
    }
}

fn schedule_nodes<T>(
    scheduled: &mut usize,
    additional: usize,
    work: &mut Vec<T>,
    nodes: impl IntoIterator<Item = T>,
) -> Result<(), InstantiationError> {
    *scheduled = scheduled
        .checked_add(additional)
        .ok_or(InstantiationError::StructureBudget)?;
    if *scheduled > INSTANCE_KEY_STRUCTURE_BUDGET {
        return Err(InstantiationError::StructureBudget);
    }
    work.extend(nodes);
    Ok(())
}

fn type_has_open_component(root: &Type) -> bool {
    let mut work = vec![root];
    while let Some(ty) = work.pop() {
        match ty {
            Type::Parameter(_) | Type::AssociatedProjection { .. } => return true,
            Type::Tuple(elements) | Type::Nominal(_, elements) => work.extend(elements),
            Type::List(element) | Type::Task(element) | Type::TaskOutcome(element) => {
                work.push(element);
            }
            Type::View { bindings, .. } => work.extend(bindings.values()),
            Type::Never
            | Type::Unit
            | Type::Bool
            | Type::Int
            | Type::Float
            | Type::Text
            | Type::Error => {}
        }
    }
    false
}

#[derive(Clone)]
struct CallSite {
    key: InstanceKey,
    function: FunctionId,
    expression: ExprId,
    span: Span,
    path: String,
}

enum VisitTask {
    Enter {
        key: InstanceKey,
        site: Option<CallSite>,
    },
    Exit {
        identity: String,
        source: FunctionId,
    },
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum VisitState {
    Visiting,
    Complete,
}

/// Plans the closed set of concrete direct-call instances reachable from the
/// selected run/test roots.
pub(crate) fn plan_instance_closure(
    program: &mir::Program,
    roots: &[FunctionId],
) -> Result<InstanceClosureOutcome, InstanceClosureError> {
    let mut tasks = roots
        .iter()
        .rev()
        .map(|root| VisitTask::Enter {
            key: InstanceKey::monomorphic(*root),
            site: None,
        })
        .collect::<Vec<_>>();
    let mut states = BTreeMap::new();
    let mut active_sources = BTreeMap::<FunctionId, String>::new();
    let mut entries = Vec::new();
    let mut instance_calls = BTreeMap::new();
    let mut call_edges = 0_usize;

    while let Some(task) = tasks.pop() {
        match task {
            VisitTask::Exit { identity, source } => {
                states.insert(identity.clone(), VisitState::Complete);
                if active_sources.get(&source) == Some(&identity) {
                    active_sources.remove(&source);
                }
            }
            VisitTask::Enter { key, site } => {
                let identity = key.canonical_identity();
                if states.contains_key(&identity) {
                    continue;
                }
                if active_sources
                    .get(&key.source())
                    .is_some_and(|active| active != &identity)
                {
                    let site = site.expect("a root cannot recurse into another instance");
                    return Ok(InstanceClosureOutcome::Unsupported(
                        InstanceClosureUnsupported {
                            kind: InstanceClosureUnsupportedKind::NonRegularRecursion,
                            function: site.function,
                            expression: Some(site.expression),
                            span: site.span,
                            path: site.path,
                        },
                    ));
                }
                if entries.len() >= INSTANCE_CLOSURE_MAX_INSTANCES {
                    let (function, expression, span, path) = site.map_or_else(
                        || {
                            let function = program
                                .function(key.source())
                                .expect("root function existence is checked below");
                            (
                                function.id,
                                None,
                                function.span,
                                "artifact.roots".to_owned(),
                            )
                        },
                        |site| (site.function, Some(site.expression), site.span, site.path),
                    );
                    return Ok(InstanceClosureOutcome::Unsupported(
                        InstanceClosureUnsupported {
                            kind: InstanceClosureUnsupportedKind::InstanceBudget,
                            function,
                            expression,
                            span,
                            path,
                        },
                    ));
                }
                let function = program
                    .function(key.source())
                    .ok_or(InstanceClosureError::MissingFunction(key.source()))?;
                require_instance_arity(function, &key)?;
                let calls = match collect_instance_calls(function, &key)? {
                    Ok(calls) => calls,
                    Err(issue) => return Ok(InstanceClosureOutcome::Unsupported(issue)),
                };
                call_edges = call_edges.saturating_add(calls.len());
                if call_edges > INSTANCE_CLOSURE_MAX_CALL_EDGES {
                    let offending = calls.last().cloned().or(site).unwrap_or(CallSite {
                        key: key.clone(),
                        function: function.id,
                        expression: ExprId::UNASSIGNED,
                        span: function.span,
                        path: format!("function[{}]", function.id.0),
                    });
                    return Ok(InstanceClosureOutcome::Unsupported(
                        InstanceClosureUnsupported {
                            kind: InstanceClosureUnsupportedKind::InstanceBudget,
                            function: offending.function,
                            expression: (offending.expression != ExprId::UNASSIGNED)
                                .then_some(offending.expression),
                            span: offending.span,
                            path: offending.path,
                        },
                    ));
                }
                active_sources.insert(key.source(), identity.clone());
                states.insert(identity.clone(), VisitState::Visiting);
                entries.push(key.clone());
                instance_calls.insert(
                    identity.clone(),
                    calls
                        .iter()
                        .map(|site| site.key.clone())
                        .collect::<Vec<_>>()
                        .into_boxed_slice(),
                );
                tasks.push(VisitTask::Exit {
                    identity,
                    source: key.source(),
                });
                tasks.extend(calls.into_iter().rev().map(|site| VisitTask::Enter {
                    key: site.key.clone(),
                    site: Some(site),
                }));
            }
        }
    }

    entries.sort_by(|left, right| {
        left.source()
            .cmp(&right.source())
            .then_with(|| left.canonical_identity().cmp(&right.canonical_identity()))
    });
    Ok(InstanceClosureOutcome::Complete(InstanceClosure {
        entries,
        calls: instance_calls,
    }))
}

fn require_instance_arity(
    function: &mir::Function,
    key: &InstanceKey,
) -> Result<(), InstanceClosureError> {
    let expected_types = function.type_parameters as usize;
    let actual_types = key.type_arguments().len();
    let expected_witnesses = function.witness_params.len();
    let actual_witnesses = key.witness_arguments().len();
    if expected_types == actual_types && expected_witnesses == actual_witnesses {
        return Ok(());
    }
    Err(InstanceClosureError::InvalidInstanceArity {
        function: function.id,
        expected_types,
        actual_types,
        expected_witnesses,
        actual_witnesses,
    })
}

fn collect_instance_calls(
    function: &mir::Function,
    key: &InstanceKey,
) -> Result<Result<Vec<CallSite>, InstanceClosureUnsupported>, InstanceClosureError> {
    let mut calls = Vec::new();
    let substitution = InstanceSubstitution::new(key);
    let result = scan_block(
        function,
        &function.body,
        &format!("function[{}].body", function.id.0),
        &substitution,
        &mut calls,
    );
    Ok(result.map(|_| calls))
}

fn instantiation_issue(
    function: &mir::Function,
    expression: &mir::Expr,
    path: &str,
) -> InstanceClosureUnsupported {
    InstanceClosureUnsupported {
        kind: InstanceClosureUnsupportedKind::Instantiation,
        function: function.id,
        expression: Some(expression.id),
        span: expression.span,
        path: path.to_owned(),
    }
}

type ScanResult = Result<bool, InstanceClosureUnsupported>;

fn scan_block(
    function: &mir::Function,
    block: &mir::Block,
    path: &str,
    substitution: &InstanceSubstitution<'_>,
    calls: &mut Vec<CallSite>,
) -> ScanResult {
    for (index, statement) in block.statements.iter().enumerate() {
        if !scan_statement(
            function,
            statement,
            &format!("{path}.statements[{index}]"),
            substitution,
            calls,
        )? {
            return Ok(false);
        }
    }
    match block.tail.as_deref() {
        Some(tail) => scan_expr(function, tail, &format!("{path}.tail"), substitution, calls),
        None => Ok(true),
    }
}

fn scan_statement(
    function: &mir::Function,
    statement: &mir::Statement,
    path: &str,
    substitution: &InstanceSubstitution<'_>,
    calls: &mut Vec<CallSite>,
) -> ScanResult {
    match &statement.kind {
        StatementKind::Let { value, .. }
        | StatementKind::LetTuple { value, .. }
        | StatementKind::Assign { value, .. }
        | StatementKind::Evaluate(value)
        | StatementKind::Assert { condition: value } => {
            scan_expr(function, value, path, substitution, calls)
        }
        StatementKind::ForRange {
            start, end, body, ..
        } => {
            if !scan_expr(
                function,
                start,
                &format!("{path}.start"),
                substitution,
                calls,
            )? || !scan_expr(function, end, &format!("{path}.end"), substitution, calls)?
            {
                return Ok(false);
            }
            let _ = scan_block(function, body, &format!("{path}.body"), substitution, calls)?;
            Ok(true)
        }
        StatementKind::Defer(cleanup) => {
            let _ = scan_block(
                function,
                cleanup,
                &format!("{path}.cleanup"),
                substitution,
                calls,
            )?;
            Ok(true)
        }
        StatementKind::Return(value) => {
            if let Some(value) = value {
                let _ = scan_expr(
                    function,
                    value,
                    &format!("{path}.value"),
                    substitution,
                    calls,
                )?;
            }
            Ok(false)
        }
    }
}

#[allow(clippy::too_many_lines)]
fn scan_expr(
    function: &mir::Function,
    expression: &mir::Expr,
    path: &str,
    substitution: &InstanceSubstitution<'_>,
    calls: &mut Vec<CallSite>,
) -> ScanResult {
    let continues = match &expression.kind {
        ExprKind::Constant(_)
        | ExprKind::Copy(_)
        | ExprKind::Move(_)
        | ExprKind::ReborrowView { .. } => true,
        ExprKind::Tuple(values)
        | ExprKind::List(values)
        | ExprKind::Record { fields: values, .. }
        | ExprKind::Variant {
            payload: values, ..
        }
        | ExprKind::TaskJoin {
            arguments: values, ..
        } => scan_exprs(function, values, path, substitution, calls)?,
        ExprKind::Unary(_, value)
        | ExprKind::Refine { value, .. }
        | ExprKind::Unrefine(value)
        | ExprKind::MakeView { value, .. }
        | ExprKind::Await { task: value, .. }
        | ExprKind::Sleep {
            milliseconds: value,
        } => scan_expr(function, value, path, substitution, calls)?,
        ExprKind::Binary(operator, left, right) => {
            if !scan_expr(function, left, &format!("{path}.left"), substitution, calls)? {
                false
            } else {
                let right = scan_expr(
                    function,
                    right,
                    &format!("{path}.right"),
                    substitution,
                    calls,
                )?;
                right || matches!(operator, mir::BinaryOp::And | mir::BinaryOp::Or)
            }
        }
        ExprKind::Block(block) => scan_block(
            function,
            block,
            &format!("{path}.block"),
            substitution,
            calls,
        )?,
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            if !scan_expr(
                function,
                condition,
                &format!("{path}.condition"),
                substitution,
                calls,
            )? {
                false
            } else {
                let then_continues = scan_block(
                    function,
                    then_branch,
                    &format!("{path}.then"),
                    substitution,
                    calls,
                )?;
                let else_continues = scan_block(
                    function,
                    else_branch,
                    &format!("{path}.else"),
                    substitution,
                    calls,
                )?;
                then_continues || else_continues
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            if !scan_expr(
                function,
                scrutinee,
                &format!("{path}.scrutinee"),
                substitution,
                calls,
            )? {
                false
            } else {
                let mut continues = false;
                for (index, arm) in arms.iter().enumerate() {
                    continues |= scan_expr(
                        function,
                        &arm.value,
                        &format!("{path}.arms[{index}].value"),
                        substitution,
                        calls,
                    )?;
                }
                continues
            }
        }
        ExprKind::Call {
            target,
            type_arguments,
            arguments,
            witnesses,
        } => {
            for (index, argument) in arguments.iter().enumerate() {
                if let CallArgument::Value(value) = argument
                    && !scan_expr(
                        function,
                        value,
                        &format!("{path}.arguments[{index}]"),
                        substitution,
                        calls,
                    )?
                {
                    return Ok(false);
                }
            }
            if let CallTarget::Direct(callee) | CallTarget::Inherent(callee) = target {
                let key = substitution
                    .call_key(*callee, type_arguments, witnesses)
                    .map_err(|_| instantiation_issue(function, expression, path))?;
                calls.push(CallSite {
                    key,
                    function: function.id,
                    expression: expression.id,
                    span: expression.span,
                    path: format!("{path}.instance"),
                });
            }
            expression.ty != Type::Never
        }
    };
    Ok(continues && expression.ty != Type::Never)
}

fn scan_exprs(
    function: &mir::Function,
    expressions: &[mir::Expr],
    path: &str,
    substitution: &InstanceSubstitution<'_>,
    calls: &mut Vec<CallSite>,
) -> ScanResult {
    for (index, expression) in expressions.iter().enumerate() {
        if !scan_expr(
            function,
            expression,
            &format!("{path}[{index}]"),
            substitution,
            calls,
        )? {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use loom_mir::{FunctionId, Type, WitnessId, WitnessRef};

    use super::{InstanceSubstitution, InstantiationError};
    use crate::{INSTANCE_KEY_STRUCTURE_BUDGET, InstanceKey, InstanceWitnessArgument};

    #[test]
    fn substitution_preflights_repeated_expansion_before_cloning() {
        let key = InstanceKey::new(
            FunctionId(0),
            vec![Type::Tuple(vec![Type::Int; 32])],
            Vec::new(),
        );
        let schema = Type::Tuple(vec![Type::Parameter(0); 8]);
        assert_eq!(
            InstanceSubstitution::new(&key).instantiate_type(&schema),
            Err(InstantiationError::StructureBudget)
        );
    }

    #[test]
    fn forwarded_and_applied_witnesses_keep_static_identity() {
        let key = InstanceKey::new(
            FunctionId(0),
            vec![Type::Int],
            vec![InstanceWitnessArgument::Concrete(WitnessId(7))],
        );
        let call = InstanceSubstitution::new(&key)
            .call_key(
                FunctionId(1),
                &[Type::Parameter(0)],
                &[WitnessRef::Apply {
                    witness: WitnessId(9),
                    arguments: vec![WitnessRef::Parameter(0)],
                }],
            )
            .expect("bounded closed instance");
        assert_eq!(call.type_arguments(), &[Type::Int]);
        assert_eq!(
            call.witness_arguments(),
            &[InstanceWitnessArgument::apply(
                WitnessId(9),
                vec![InstanceWitnessArgument::Concrete(WitnessId(7))]
            )]
        );
    }

    #[test]
    fn a_maximum_sized_actual_replaces_its_parameter_without_double_counting() {
        let key = InstanceKey::new(
            FunctionId(0),
            vec![Type::Tuple(vec![
                Type::Int;
                INSTANCE_KEY_STRUCTURE_BUDGET - 1
            ])],
            Vec::new(),
        );
        assert!(
            InstanceSubstitution::new(&key)
                .instantiate_type(&Type::Parameter(0))
                .is_ok()
        );
    }
}
