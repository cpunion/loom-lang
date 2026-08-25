//! Closed-world selection of compiler-private native storage.
//!
//! These plans are deliberately conservative. A local is selected only when
//! every use in its synchronous MIR body can be lowered without ever exposing
//! the private representation through the universal `Value` ABI.

use std::collections::BTreeSet;

use loom_mir::{
    Block, Builtin, CallArgument, CallTarget, Expr, ExprKind, Function, LocalId, MatchArm, Pattern,
    Place, Program, StatementKind, Type, TypeId,
};

/// Native, uniquely-owned `{ data, len, capacity }` storage for local
/// `List[Int]` values.
#[derive(Clone, Debug, Default)]
pub(crate) struct NativeIntListPlan {
    locals: BTreeSet<LocalId>,
    option: Option<TypeId>,
}

impl NativeIntListPlan {
    #[must_use]
    pub(crate) fn analyze(program: &Program, function: &Function) -> Self {
        if function.is_async {
            return Self::default();
        }
        let option = program.prelude.option;
        let locals = function
            .locals
            .iter()
            .filter(|local| local.ty == Type::List(Box::new(Type::Int)))
            .filter_map(|local| {
                let mut scanner = IntListUseScanner::new(local.id, option);
                scanner.scan_block(&function.body, true);
                (scanner.valid && scanner.initializers == 1).then_some(local.id)
            })
            .collect();
        Self { locals, option }
    }

    #[must_use]
    pub(crate) fn contains(&self, local: LocalId) -> bool {
        self.locals.contains(&local)
    }

    pub(crate) fn locals(&self) -> impl Iterator<Item = LocalId> + '_ {
        self.locals.iter().copied()
    }

    /// Recognizes the only `List.get` consumer accepted by the plan: a direct,
    /// exhaustive `Option[Int]` match. The analyzer has already rejected any
    /// receiver mutation nested in the index expression.
    #[must_use]
    pub(crate) fn direct_get_match<'a>(
        &self,
        scrutinee: &'a Expr,
        arms: &'a [MatchArm],
    ) -> Option<NativeIntListGetMatch<'a>> {
        let (local, index) = int_list_get_call(scrutinee)?;
        if !self.contains(local) {
            return None;
        }
        option_match(self.option?, arms).map(|arms| NativeIntListGetMatch {
            local,
            index,
            some: arms.some,
            none: arms.none,
            some_binding: arms.some_binding,
        })
    }
}

#[derive(Clone, Copy)]
pub(crate) struct NativeIntListGetMatch<'a> {
    pub(crate) local: LocalId,
    pub(crate) index: &'a Expr,
    pub(crate) some: &'a MatchArm,
    pub(crate) none: &'a MatchArm,
    pub(crate) some_binding: Option<LocalId>,
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

struct IntListUseScanner {
    local: LocalId,
    option: Option<TypeId>,
    initializers: usize,
    valid: bool,
}

impl IntListUseScanner {
    fn new(local: LocalId, option: Option<TypeId>) -> Self {
        Self {
            local,
            option,
            initializers: 0,
            valid: true,
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
            ExprKind::WaitFd { descriptor, .. } => self.scan_expr(descriptor),
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
            StatementKind::Let {
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
        ExprKind::WaitFd { descriptor, .. } => expr_references_local(descriptor, local),
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
    use super::{NativeIntListPlan, option_match};
    use loom_mir::{
        Block, CallPlan, Expr, ExprKind, Function, FunctionId, LocalDecl, LocalId, MatchArm,
        Pattern, PreludeIds, Program, Statement, StatementKind, Type, TypeId, VariantId,
    };

    const LIST: LocalId = LocalId(0);
    const VALUE: LocalId = LocalId(1);
    const OPTION: TypeId = TypeId(7);

    fn expression(kind: ExprKind, ty: Type) -> Expr {
        Expr {
            kind,
            ty,
            span: Default::default(),
        }
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
}
