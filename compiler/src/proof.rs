//! A deliberately bounded proof fragment, not a runtime postcondition monitor.
//! Integers are mathematical on normal returns: emitted arithmetic traps instead
//! of wrapping. Unsupported operations and exhausted budgets reject the proof.

use crate::model::{Binary, Diagnostic, Span, Type, Unary, checked as c};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Linear {
    constant: i128,
    terms: BTreeMap<usize, i128>,
}
impl Linear {
    fn constant(value: i128) -> Self {
        Self {
            constant: value,
            terms: BTreeMap::new(),
        }
    }
    fn variable(id: usize) -> Self {
        Self {
            constant: 0,
            terms: BTreeMap::from([(id, 1)]),
        }
    }
    fn scale(mut self, factor: i128) -> Option<Self> {
        self.constant = self.constant.checked_mul(factor)?;
        for v in self.terms.values_mut() {
            *v = v.checked_mul(factor)?;
        }
        self.terms.retain(|_, v| *v != 0);
        Some(self)
    }
    fn add(mut self, other: Self) -> Option<Self> {
        self.constant = self.constant.checked_add(other.constant)?;
        for (key, value) in other.terms {
            let term = self.terms.entry(key).or_default();
            *term = term.checked_add(value)?;
        }
        self.terms.retain(|_, v| *v != 0);
        Some(self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rel {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}
impl Rel {
    fn neg(self) -> Self {
        match self {
            Self::Eq => Self::Ne,
            Self::Ne => Self::Eq,
            Self::Lt => Self::Ge,
            Self::Le => Self::Gt,
            Self::Gt => Self::Le,
            Self::Ge => Self::Lt,
        }
    }
    fn flip(self) -> Self {
        match self {
            Self::Lt => Self::Gt,
            Self::Le => Self::Ge,
            Self::Gt => Self::Lt,
            Self::Ge => Self::Le,
            x => x,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Pred {
    Constant(bool),
    Variable(usize),
    Compare(Rel, Linear),
    Equal(Box<Pred>, Box<Pred>),
    Not(Box<Pred>),
    And(Box<Pred>, Box<Pred>),
    Or(Box<Pred>, Box<Pred>),
}
impl Pred {
    fn neg(self) -> Self {
        match self {
            Self::Constant(v) => Self::Constant(!v),
            Self::Compare(r, e) => Self::Compare(r.neg(), e),
            Self::Not(p) => *p,
            Self::And(a, b) => Self::Or(Box::new(a.neg()), Box::new(b.neg())),
            Self::Or(a, b) => Self::And(Box::new(a.neg()), Box::new(b.neg())),
            p => Self::Not(Box::new(p)),
        }
    }
}

#[derive(Clone)]
enum Value {
    Int(Linear),
    Bool(Pred),
    Unit,
}
#[derive(Clone)]
struct State {
    locals: Vec<Option<Value>>,
    facts: Vec<Pred>,
}
impl State {
    fn assume(&mut self, fact: Pred) {
        match fact {
            Pred::And(a, b) => {
                self.assume(*a);
                self.assume(*b);
            }
            p => self.facts.push(p),
        }
    }
}

fn error(span: Span) -> Diagnostic {
    Diagnostic::new(
        span,
        "required postcondition is not proved by the seed's bounded scalar prover",
    )
}

/// Classify a constant boundary without executing effects or assuming that an
/// overflowing arithmetic operation completed. None retains a runtime check.
pub fn classify_refinement(value: &c::Expr, predicate: &c::Expr, self_slot: usize) -> Option<bool> {
    let mut state = State {
        locals: vec![None; self_slot + 1],
        facts: Vec::new(),
    };
    defined(value, &state).ok()?;
    let value = pure(value, &state).ok()?;
    if !matches!(value, Value::Int(_)) {
        return None;
    }
    state.locals[self_slot] = Some(value);
    defined(predicate, &state).ok()?;
    let Value::Bool(predicate) = pure(predicate, &state).ok()? else {
        return None;
    };
    if holds(&predicate, &state.facts) {
        Some(true)
    } else if holds(&predicate.neg(), &state.facts) {
        Some(false)
    } else {
        None
    }
}

pub fn prove(
    function: &c::Function,
    ensures: &[c::Expr],
    result_slot: usize,
) -> Result<(), Diagnostic> {
    let mut state = State {
        locals: vec![None; result_slot + 1],
        facts: Vec::new(),
    };
    for (id, ty) in function.params.iter().enumerate() {
        state.locals[id] = Some(match ty {
            Type::Int => Value::Int(Linear::variable(id)),
            Type::Bool => Value::Bool(Pred::Variable(id)),
            Type::Unit => Value::Unit,
            Type::Text | Type::Bytes | Type::List(_) | Type::Data(_) | Type::Parameter(_) => {
                return Err(error(function.span));
            }
        });
    }
    let mut proof = Proof {
        ensures,
        result_slot,
        remaining: 4096,
    };
    // Contract predicates have no statements or function returns in this fragment.
    for requires in &function.requires {
        let Value::Bool(fact) = pure(requires, &state)? else {
            return Err(error(requires.span));
        };
        state.assume(fact);
    }
    for (state, result) in proof.block(&function.body, state)? {
        proof.returned(state, result, function.span)?;
    }
    Ok(())
}

struct Proof<'a> {
    ensures: &'a [c::Expr],
    result_slot: usize,
    remaining: usize,
}
impl Proof<'_> {
    fn tick(&mut self, span: Span) -> Result<(), Diagnostic> {
        self.remaining = self.remaining.checked_sub(1).ok_or_else(|| error(span))?;
        Ok(())
    }
    fn returned(&mut self, mut state: State, result: Value, span: Span) -> Result<(), Diagnostic> {
        self.tick(span)?;
        state.locals[self.result_slot] = Some(result);
        for ensures in self.ensures {
            defined(ensures, &state)?;
            let Value::Bool(predicate) = pure(ensures, &state)? else {
                return Err(error(ensures.span));
            };
            if !holds(&predicate, &state.facts) {
                return Err(error(ensures.span));
            }
        }
        Ok(())
    }
    fn block(&mut self, block: &c::Block, state: State) -> Result<Vec<(State, Value)>, Diagnostic> {
        let mut states = vec![state];
        for stmt in &block.statements {
            let mut next = Vec::new();
            for state in states {
                self.tick(stmt.span)?;
                use c::StmtKind as S;
                match &stmt.kind {
                    S::Let { local, value } | S::Assign { local, value } => {
                        for (mut state, value) in self.expr(value, state)? {
                            state.locals[*local] = Some(value);
                            next.push(state);
                        }
                    }
                    S::Return(value) => {
                        if let Some(value) = value {
                            for (state, result) in self.expr(value, state)? {
                                self.returned(state, result, stmt.span)?;
                            }
                        } else {
                            self.returned(state, Value::Unit, stmt.span)?;
                        }
                    }
                    S::Assert(value) => {
                        for (mut state, value) in self.expr(value, state)? {
                            let Value::Bool(fact) = value else {
                                return Err(error(stmt.span));
                            };
                            state.assume(fact);
                            next.push(state);
                        }
                    }
                    S::Discard(value) | S::Expr(value) => {
                        next.extend(self.expr(value, state)?.into_iter().map(|(s, _)| s))
                    }
                    S::While { .. } => return Err(error(stmt.span)),
                }
            }
            states = next;
        }
        let mut output = Vec::new();
        for state in states {
            if let Some(tail) = &block.tail {
                output.extend(self.expr(tail, state)?);
            } else {
                output.push((state, Value::Unit));
            }
        }
        Ok(output)
    }
    fn expr(&mut self, expr: &c::Expr, state: State) -> Result<Vec<(State, Value)>, Diagnostic> {
        self.tick(expr.span)?;
        match &expr.kind {
            c::ExprKind::If {
                condition,
                then_body,
                else_body,
            } => {
                let mut result = Vec::new();
                for (state, value) in self.expr(condition, state)? {
                    let Value::Bool(fact) = value else {
                        return Err(error(expr.span));
                    };
                    let mut yes = state.clone();
                    yes.assume(fact.clone());
                    result.extend(self.block(then_body, yes)?);
                    let mut no = state;
                    no.assume(fact.neg());
                    if let Some(body) = else_body {
                        result.extend(self.block(body, no)?);
                    } else {
                        result.push((no, Value::Unit));
                    }
                }
                Ok(result)
            }
            c::ExprKind::Unary(op, inner) => self
                .expr(inner, state)?
                .into_iter()
                .map(|(mut s, v)| {
                    let value = unary(*op, v).ok_or_else(|| error(expr.span))?;
                    bounded(&value, &mut s, expr.span)?;
                    Ok((s, value))
                })
                .collect(),
            c::ExprKind::Binary(op, left, right) => {
                let mut result = Vec::new();
                for (state, a) in self.expr(left, state)? {
                    if matches!(op, Binary::And | Binary::Or) {
                        let Value::Bool(fact) = a else {
                            return Err(error(expr.span));
                        };
                        let is_and = *op == Binary::And;
                        let mut short = state.clone();
                        short.assume(if is_and {
                            fact.clone().neg()
                        } else {
                            fact.clone()
                        });
                        result.push((short, Value::Bool(Pred::Constant(!is_and))));
                        let mut next = state;
                        next.assume(if is_and { fact } else { fact.neg() });
                        result.extend(self.expr(right, next)?);
                    } else {
                        for (mut state, b) in self.expr(right, state)? {
                            let value =
                                binary(*op, a.clone(), b).ok_or_else(|| error(expr.span))?;
                            bounded(&value, &mut state, expr.span)?;
                            result.push((state, value));
                        }
                    }
                }
                Ok(result)
            }
            _ => {
                let value = pure(expr, &state)?;
                Ok(vec![(state, value)])
            }
        }
    }
}

fn bounded(value: &Value, state: &mut State, span: Span) -> Result<(), Diagnostic> {
    if let Value::Int(value) = value {
        for (rel, bound) in [(Rel::Ge, i64::MIN), (Rel::Le, i64::MAX)] {
            let term = value
                .clone()
                .add(Linear::constant(-(bound as i128)))
                .ok_or_else(|| error(span))?;
            state.assume(Pred::Compare(rel, term));
        }
    }
    Ok(())
}

// Unlike body arithmetic, arithmetic mentioned only in ensures has not run.
// Prove it is defined; do not assume that a hypothetical overflow check passed.
fn defined(expr: &c::Expr, state: &State) -> Result<(), Diagnostic> {
    if holds(&Pred::Constant(false), &state.facts) {
        return Ok(());
    }
    match &expr.kind {
        c::ExprKind::Unary(_, inner) | c::ExprKind::Coerce(inner) => defined(inner, state)?,
        c::ExprKind::Binary(op, a, b) => {
            defined(a, state)?;
            let mut branch = state.clone();
            if matches!(op, Binary::And | Binary::Or) {
                let Value::Bool(fact) = pure(a, state)? else {
                    return Err(error(expr.span));
                };
                branch.assume(if *op == Binary::And { fact } else { fact.neg() });
            }
            defined(b, &branch)?;
        }
        _ => (),
    }
    if matches!(
        expr.kind,
        c::ExprKind::Unary(Unary::Neg, _)
            | c::ExprKind::Binary(
                Binary::Add | Binary::Sub | Binary::Mul | Binary::Div | Binary::Rem,
                _,
                _
            )
    ) {
        let Value::Int(value) = pure(expr, state)? else {
            return Err(error(expr.span));
        };
        if !range(&value, &state.facts).is_some_and(|(low, high)| {
            low > high || (low >= i64::MIN as i128 && high <= i64::MAX as i128)
        }) {
            return Err(error(expr.span));
        }
    }
    Ok(())
}

fn pure(expr: &c::Expr, state: &State) -> Result<Value, Diagnostic> {
    use c::ExprKind as E;
    let value = match &expr.kind {
        E::Int(n) => Some(Value::Int(Linear::constant(*n as i128))),
        E::Bool(v) => Some(Value::Bool(Pred::Constant(*v))),
        E::Local(id) => state.locals.get(*id).cloned().flatten(),
        E::Coerce(value) => Some(pure(value, state)?),
        E::Unary(op, e) => unary(*op, pure(e, state)?),
        E::Binary(op, a, b) => binary(*op, pure(a, state)?, pure(b, state)?),
        E::Text(_)
        | E::Primitive(..)
        | E::Call(..)
        | E::If { .. }
        | E::Record(_)
        | E::Field(..)
        | E::Variant { .. }
        | E::Match { .. }
        | E::Block(_) => None,
    };
    value.ok_or_else(|| error(expr.span))
}

fn unary(op: Unary, value: Value) -> Option<Value> {
    match (op, value) {
        (Unary::Neg, Value::Int(value)) => Some(Value::Int(value.scale(-1)?)),
        (Unary::Not, Value::Bool(value)) => Some(Value::Bool(value.neg())),
        _ => None,
    }
}
fn binary(op: Binary, a: Value, b: Value) -> Option<Value> {
    use Binary as B;
    match (a, b) {
        (Value::Int(a), Value::Int(b)) => Some(match op {
            B::Add => Value::Int(a.add(b)?),
            B::Sub => Value::Int(a.add(b.scale(-1)?)?),
            B::Mul if a.terms.is_empty() => Value::Int(b.scale(a.constant)?),
            B::Mul if b.terms.is_empty() => Value::Int(a.scale(b.constant)?),
            B::Div | B::Rem if a.terms.is_empty() && b.terms.is_empty() => {
                let a = i64::try_from(a.constant).ok()?;
                let b = i64::try_from(b.constant).ok()?;
                let value = if op == B::Div {
                    a.checked_div(b)?
                } else {
                    a.checked_rem(b)?
                };
                Value::Int(Linear::constant(value as i128))
            }
            B::Eq | B::Ne | B::Lt | B::Le | B::Gt | B::Ge => {
                let rel = match op {
                    B::Eq => Rel::Eq,
                    B::Ne => Rel::Ne,
                    B::Lt => Rel::Lt,
                    B::Le => Rel::Le,
                    B::Gt => Rel::Gt,
                    _ => Rel::Ge,
                };
                Value::Bool(Pred::Compare(rel, a.add(b.scale(-1)?)?))
            }
            _ => return None,
        }),
        (Value::Bool(a), Value::Bool(b)) => Some(Value::Bool(match op {
            B::And => Pred::And(Box::new(a), Box::new(b)),
            B::Or => Pred::Or(Box::new(a), Box::new(b)),
            B::Eq | B::Ne => {
                let same = if a == b {
                    Pred::Constant(true)
                } else if a == b.clone().neg() {
                    Pred::Constant(false)
                } else {
                    Pred::Equal(Box::new(a), Box::new(b))
                };
                if op == B::Eq { same } else { same.neg() }
            }
            _ => return None,
        })),
        _ => None,
    }
}

fn tighten(rel: Rel, value: i128, low: &mut i128, high: &mut i128) {
    match rel {
        Rel::Eq => {
            *low = (*low).max(value);
            *high = (*high).min(value);
        }
        Rel::Lt => {
            if let Some(v) = value.checked_sub(1) {
                *high = (*high).min(v);
            }
        }
        Rel::Le => *high = (*high).min(value),
        Rel::Gt => {
            if let Some(v) = value.checked_add(1) {
                *low = (*low).max(v);
            }
        }
        Rel::Ge => *low = (*low).max(value),
        Rel::Ne => (),
    }
}

fn range(linear: &Linear, facts: &[Pred]) -> Option<(i128, i128)> {
    let (mut low, mut high) = (linear.constant, linear.constant);
    for (id, coefficient) in &linear.terms {
        let (mut a, mut b) = (i64::MIN as i128, i64::MAX as i128);
        for fact in facts {
            if let Pred::Compare(rel, term) = fact {
                if term.terms.len() == 1 {
                    if let Some(factor) = term.terms.get(id) {
                        if *factor == 1 {
                            tighten(*rel, term.constant.checked_neg()?, &mut a, &mut b);
                        }
                        if *factor == -1 {
                            tighten(rel.flip(), term.constant, &mut a, &mut b);
                        }
                    }
                }
            }
        }
        if a > b {
            return Some((1, 0));
        }
        let (a, b) = if *coefficient >= 0 { (a, b) } else { (b, a) };
        low = low.checked_add(a.checked_mul(*coefficient)?)?;
        high = high.checked_add(b.checked_mul(*coefficient)?)?;
    }
    for fact in facts {
        if let Pred::Compare(rel, term) = fact {
            if term.terms == linear.terms {
                tighten(
                    *rel,
                    linear.constant.checked_sub(term.constant)?,
                    &mut low,
                    &mut high,
                );
            }
        }
    }
    Some((low, high))
}

fn interval_proves(rel: Rel, low: i128, high: i128) -> bool {
    low > high
        || match rel {
            Rel::Eq => low == 0 && high == 0,
            Rel::Ne => high < 0 || low > 0,
            Rel::Lt => high < 0,
            Rel::Le => high <= 0,
            Rel::Gt => low > 0,
            Rel::Ge => low >= 0,
        }
}

fn holds(predicate: &Pred, facts: &[Pred]) -> bool {
    // Every infeasibility rule is one-sided: failing to find a contradiction
    // can reject a valid proof, but cannot certify an invalid normal return.
    let contradictory = facts.iter().any(|f| {
        matches!(f, Pred::Constant(false))
            || facts.contains(&f.clone().neg())
            || if let Pred::Compare(rel, value) = f {
                range(value, facts).is_some_and(|(a, b)| a > b || interval_proves(rel.neg(), a, b))
            } else {
                false
            }
    });
    if contradictory || facts.contains(predicate) {
        return true;
    }
    match predicate {
        Pred::Constant(value) => *value,
        Pred::Compare(rel, value) => {
            range(value, facts).is_some_and(|(a, b)| interval_proves(*rel, a, b))
        }
        Pred::And(a, b) => holds(a, facts) && holds(b, facts),
        Pred::Or(a, b) => holds(a, facts) || holds(b, facts),
        Pred::Equal(a, b) => {
            (holds(a, facts) && holds(b, facts))
                || (holds(&a.clone().neg(), facts) && holds(&b.clone().neg(), facts))
        }
        Pred::Variable(_) | Pred::Not(_) => false,
    }
}
