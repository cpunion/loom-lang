//! Closed, deliberately small proof domain for checked construction and
//! removable contract checks.
//!
//! This is not a general theorem prover.  It folds closed scalar expressions,
//! propagates immutable symbolic terms, records facts established by contracts,
//! branches and assertions, and proves direct boolean/conjunctive consequences
//! plus simple ordered-bound implications.  Anything outside this domain stays
//! on the ordinary runtime-checked `Result` path.

use std::cmp::Ordering;
use std::collections::BTreeSet;

use loom_hir::DefId;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum ProofRoot {
    Param(u32),
    Local(u32),
    /// A value-producing expression whose internals are opaque to the proof
    /// domain. The body component keeps independently allocated HIR arenas
    /// distinct.
    Expression {
        body: u32,
        expression: u32,
    },
    SelfValue,
    ResultValue,
    OldParam(u32),
    OldSelf,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct ProofPlace {
    pub(crate) root: ProofRoot,
    pub(crate) fields: Vec<DefId>,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum ProofConstant {
    Bool(bool),
    Int(i64),
    Float(u64),
    Text(String),
    Unit,
}

impl ProofConstant {
    fn float(value: f64) -> Self {
        Self::Float(value.to_bits())
    }

    fn as_float(&self) -> Option<f64> {
        match self {
            Self::Float(bits) => Some(f64::from_bits(*bits)),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum ProofUnary {
    Negate,
    Not,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum ProofBinary {
    Add,
    Subtract,
    Multiply,
    Divide,
    Equal,
    NotEqual,
    Less,
    LessEqual,
    And,
    Or,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum ProofTerm {
    Unknown,
    Constant(ProofConstant),
    Place(ProofPlace),
    Tuple(Vec<Self>),
    Record {
        definition: DefId,
        fields: Vec<(DefId, Self)>,
    },
    Variant {
        owner: DefId,
        variant: DefId,
        payload: Vec<Self>,
    },
    Refined {
        definition: DefId,
        value: Box<Self>,
    },
    Field(Box<Self>, DefId),
    Unary(ProofUnary, Box<Self>),
    Binary(ProofBinary, Box<Self>, Box<Self>),
    IsFinite(Box<Self>),
}

impl ProofTerm {
    pub(crate) const fn bool(value: bool) -> Self {
        Self::Constant(ProofConstant::Bool(value))
    }

    pub(crate) const fn int(value: i64) -> Self {
        Self::Constant(ProofConstant::Int(value))
    }

    pub(crate) fn float(value: f64) -> Self {
        Self::Constant(ProofConstant::float(value))
    }

    pub(crate) fn text(value: String) -> Self {
        Self::Constant(ProofConstant::Text(value))
    }

    pub(crate) const fn unit() -> Self {
        Self::Constant(ProofConstant::Unit)
    }

    pub(crate) fn is_known(&self) -> bool {
        match self {
            Self::Unknown => false,
            Self::Tuple(values) => values.iter().all(Self::is_known),
            Self::Record { fields, .. } => fields.iter().all(|(_, value)| value.is_known()),
            Self::Variant { payload, .. } => payload.iter().all(Self::is_known),
            Self::Refined { value, .. }
            | Self::Field(value, _)
            | Self::Unary(_, value)
            | Self::IsFinite(value) => value.is_known(),
            Self::Binary(_, left, right) => left.is_known() && right.is_known(),
            Self::Constant(_) | Self::Place(_) => true,
        }
    }

    pub(crate) fn contains_place(&self, place: &ProofPlace) -> bool {
        match self {
            Self::Place(candidate) => places_overlap(candidate, place),
            Self::Tuple(values) => values.iter().any(|value| value.contains_place(place)),
            Self::Record { fields, .. } => {
                fields.iter().any(|(_, value)| value.contains_place(place))
            }
            Self::Variant { payload, .. } => {
                payload.iter().any(|value| value.contains_place(place))
            }
            Self::Refined { value, .. }
            | Self::Field(value, _)
            | Self::Unary(_, value)
            | Self::IsFinite(value) => value.contains_place(place),
            Self::Binary(_, left, right) => {
                left.contains_place(place) || right.contains_place(place)
            }
            Self::Unknown | Self::Constant(_) => false,
        }
    }

    pub(crate) fn unrefine(self) -> Self {
        match self {
            Self::Refined { value, .. } => value.unrefine(),
            value => value,
        }
    }

    pub(crate) fn field(self, field: DefId) -> Self {
        match self.unrefine() {
            Self::Record { fields, .. } => fields
                .into_iter()
                .find_map(|(candidate, value)| (candidate == field).then_some(value))
                .unwrap_or(Self::Unknown),
            Self::Place(mut place) => {
                place.fields.push(field);
                Self::Place(place)
            }
            Self::Unknown => Self::Unknown,
            value => Self::Field(Box::new(value), field),
        }
    }

    pub(crate) fn unary(operator: ProofUnary, value: Self) -> Self {
        if value == Self::Unknown {
            return Self::Unknown;
        }
        match (operator, &value) {
            (ProofUnary::Not, Self::Constant(ProofConstant::Bool(value))) => Self::bool(!value),
            (ProofUnary::Negate, Self::Constant(ProofConstant::Int(value))) => {
                value.checked_neg().map_or(Self::Unknown, Self::int)
            }
            (ProofUnary::Negate, Self::Constant(constant)) => constant
                .as_float()
                .map_or(Self::Unknown, |value| Self::float(-value)),
            (ProofUnary::Not, Self::Unary(ProofUnary::Not, inner)) => *inner.clone(),
            _ => Self::Unary(operator, Box::new(value)),
        }
    }

    pub(crate) fn binary(operator: ProofBinary, left: Self, right: Self) -> Self {
        if (left == Self::Unknown || right == Self::Unknown)
            && !matches!(operator, ProofBinary::And | ProofBinary::Or)
        {
            return Self::Unknown;
        }

        // Canonical spellings make source-equivalent facts compare equal.
        let (operator, left, right) = match operator {
            ProofBinary::Equal | ProofBinary::NotEqual if right < left => (operator, right, left),
            _ => (operator, left, right),
        };

        if let Some(value) = fold_binary(operator, &left, &right) {
            return value;
        }
        match operator {
            ProofBinary::And => match (&left, &right) {
                (Self::Constant(ProofConstant::Bool(false)), _)
                | (_, Self::Constant(ProofConstant::Bool(false))) => Self::bool(false),
                (Self::Constant(ProofConstant::Bool(true)), _) => right,
                (_, Self::Constant(ProofConstant::Bool(true))) => left,
                _ if left == right => left,
                _ => Self::Binary(operator, Box::new(left), Box::new(right)),
            },
            ProofBinary::Or => match (&left, &right) {
                (Self::Constant(ProofConstant::Bool(true)), _)
                | (_, Self::Constant(ProofConstant::Bool(true))) => Self::bool(true),
                (Self::Constant(ProofConstant::Bool(false)), _) => right,
                (_, Self::Constant(ProofConstant::Bool(false))) => left,
                _ if left == right => left,
                _ => Self::Binary(operator, Box::new(left), Box::new(right)),
            },
            _ => Self::Binary(operator, Box::new(left), Box::new(right)),
        }
    }

    pub(crate) fn is_finite(value: Self) -> Self {
        if value == Self::Unknown {
            return Self::Unknown;
        }
        if let Self::Constant(constant) = &value
            && let Some(value) = constant.as_float()
        {
            return Self::bool(value.is_finite());
        }
        Self::IsFinite(Box::new(value))
    }
}

fn fold_binary(operator: ProofBinary, left: &ProofTerm, right: &ProofTerm) -> Option<ProofTerm> {
    let (ProofTerm::Constant(left), ProofTerm::Constant(right)) = (left, right) else {
        return None;
    };
    match (operator, left, right) {
        (ProofBinary::Add, ProofConstant::Int(left), ProofConstant::Int(right)) => {
            left.checked_add(*right).map(ProofTerm::int)
        }
        (ProofBinary::Subtract, ProofConstant::Int(left), ProofConstant::Int(right)) => {
            left.checked_sub(*right).map(ProofTerm::int)
        }
        (ProofBinary::Multiply, ProofConstant::Int(left), ProofConstant::Int(right)) => {
            left.checked_mul(*right).map(ProofTerm::int)
        }
        (ProofBinary::Divide, ProofConstant::Int(left), ProofConstant::Int(right)) => {
            left.checked_div(*right).map(ProofTerm::int)
        }
        (ProofBinary::Add, left, right) => {
            Some(ProofTerm::float(left.as_float()? + right.as_float()?))
        }
        (ProofBinary::Subtract, left, right) => {
            Some(ProofTerm::float(left.as_float()? - right.as_float()?))
        }
        (ProofBinary::Multiply, left, right) => {
            Some(ProofTerm::float(left.as_float()? * right.as_float()?))
        }
        (ProofBinary::Divide, left, right) => {
            Some(ProofTerm::float(left.as_float()? / right.as_float()?))
        }
        (ProofBinary::Equal, left, right) => Some(ProofTerm::bool(constants_equal(left, right))),
        (ProofBinary::NotEqual, left, right) => {
            Some(ProofTerm::bool(!constants_equal(left, right)))
        }
        (ProofBinary::Less, ProofConstant::Int(left), ProofConstant::Int(right)) => {
            Some(ProofTerm::bool(left < right))
        }
        (ProofBinary::LessEqual, ProofConstant::Int(left), ProofConstant::Int(right)) => {
            Some(ProofTerm::bool(left <= right))
        }
        (ProofBinary::Less, left, right) => {
            Some(ProofTerm::bool(left.as_float()? < right.as_float()?))
        }
        (ProofBinary::LessEqual, left, right) => {
            Some(ProofTerm::bool(left.as_float()? <= right.as_float()?))
        }
        (ProofBinary::And, ProofConstant::Bool(left), ProofConstant::Bool(right)) => {
            Some(ProofTerm::bool(*left && *right))
        }
        (ProofBinary::Or, ProofConstant::Bool(left), ProofConstant::Bool(right)) => {
            Some(ProofTerm::bool(*left || *right))
        }
        _ => None,
    }
}

#[allow(clippy::float_cmp)] // Implements Loom's exact IEEE equality semantics.
fn constants_equal(left: &ProofConstant, right: &ProofConstant) -> bool {
    match (left, right) {
        (ProofConstant::Float(left), ProofConstant::Float(right)) => {
            f64::from_bits(*left) == f64::from_bits(*right)
        }
        _ => left == right,
    }
}

fn places_overlap(left: &ProofPlace, right: &ProofPlace) -> bool {
    left.root == right.root
        && (left.fields.starts_with(&right.fields) || right.fields.starts_with(&left.fields))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProofResult {
    Proven,
    Disproven,
    Unknown,
}

impl ProofResult {
    fn not(self) -> Self {
        match self {
            Self::Proven => Self::Disproven,
            Self::Disproven => Self::Proven,
            Self::Unknown => Self::Unknown,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ProofFacts {
    truths: BTreeSet<ProofTerm>,
    falsehoods: BTreeSet<ProofTerm>,
}

impl ProofFacts {
    pub(crate) fn assume(&mut self, term: ProofTerm, truth: bool) {
        if term == ProofTerm::Unknown {
            return;
        }
        match (&term, truth) {
            (ProofTerm::Constant(ProofConstant::Bool(_)), _) => {}
            (ProofTerm::Unary(ProofUnary::Not, inner), truth) => {
                self.assume(*inner.clone(), !truth);
            }
            (ProofTerm::Binary(ProofBinary::And, left, right), true)
            | (ProofTerm::Binary(ProofBinary::Or, left, right), false) => {
                self.assume(*left.clone(), truth);
                self.assume(*right.clone(), truth);
            }
            _ if truth => {
                self.falsehoods.remove(&term);
                self.truths.insert(term);
            }
            _ => {
                self.truths.remove(&term);
                self.falsehoods.insert(term);
            }
        }
    }

    pub(crate) fn prove(&self, term: &ProofTerm) -> ProofResult {
        match term {
            ProofTerm::Unknown => ProofResult::Unknown,
            ProofTerm::Constant(ProofConstant::Bool(true)) => ProofResult::Proven,
            ProofTerm::Constant(ProofConstant::Bool(false)) => ProofResult::Disproven,
            ProofTerm::Unary(ProofUnary::Not, value) => self.prove(value).not(),
            ProofTerm::Binary(ProofBinary::And, left, right) => {
                match (self.prove(left), self.prove(right)) {
                    (ProofResult::Disproven, _) | (_, ProofResult::Disproven) => {
                        ProofResult::Disproven
                    }
                    (ProofResult::Proven, ProofResult::Proven) => ProofResult::Proven,
                    _ => self.exact_or_implied(term),
                }
            }
            ProofTerm::Binary(ProofBinary::Or, left, right) => {
                match (self.prove(left), self.prove(right)) {
                    (ProofResult::Proven, _) | (_, ProofResult::Proven) => ProofResult::Proven,
                    (ProofResult::Disproven, ProofResult::Disproven) => ProofResult::Disproven,
                    _ => self.exact_or_implied(term),
                }
            }
            _ => self.exact_or_implied(term),
        }
    }

    fn exact_or_implied(&self, term: &ProofTerm) -> ProofResult {
        if self.truths.contains(term) {
            return ProofResult::Proven;
        }
        if self.falsehoods.contains(term) {
            return ProofResult::Disproven;
        }
        if self
            .truths
            .iter()
            .any(|known| comparison_implies(known, term))
        {
            return ProofResult::Proven;
        }
        ProofResult::Unknown
    }

    pub(crate) fn invalidate(&mut self, place: &ProofPlace) {
        self.truths.retain(|term| !term.contains_place(place));
        self.falsehoods.retain(|term| !term.contains_place(place));
    }

    pub(crate) fn intersection(states: impl IntoIterator<Item = Self>) -> Self {
        let mut states = states.into_iter();
        let Some(mut result) = states.next() else {
            return Self::default();
        };
        for state in states {
            result.truths.retain(|term| state.truths.contains(term));
            result
                .falsehoods
                .retain(|term| state.falsehoods.contains(term));
        }
        result
    }
}

#[derive(Clone, Copy)]
enum BoundKind {
    Lower { inclusive: bool },
    Upper { inclusive: bool },
    Equal,
}

struct Comparison<'term> {
    subject: &'term ProofTerm,
    bound: NumericBound,
    kind: BoundKind,
}

#[derive(Clone, Copy)]
enum NumericBound {
    Int(i64),
    Float(f64),
}

impl NumericBound {
    fn compare(self, other: Self) -> Option<Ordering> {
        match (self, other) {
            (Self::Int(left), Self::Int(right)) => Some(left.cmp(&right)),
            (Self::Float(left), Self::Float(right)) => left.partial_cmp(&right),
            _ => None,
        }
    }
}

fn comparison(term: &ProofTerm) -> Option<Comparison<'_>> {
    let ProofTerm::Binary(operator, left, right) = term else {
        return None;
    };
    let left_bound = numeric_bound(left);
    let right_bound = numeric_bound(right);
    match (*operator, left_bound, right_bound) {
        (ProofBinary::Equal, None, Some(bound)) => Some(Comparison {
            subject: left,
            bound,
            kind: BoundKind::Equal,
        }),
        (ProofBinary::Equal, Some(bound), None) => Some(Comparison {
            subject: right,
            bound,
            kind: BoundKind::Equal,
        }),
        (ProofBinary::Less, None, Some(bound)) => Some(Comparison {
            subject: left,
            bound,
            kind: BoundKind::Upper { inclusive: false },
        }),
        (ProofBinary::LessEqual, None, Some(bound)) => Some(Comparison {
            subject: left,
            bound,
            kind: BoundKind::Upper { inclusive: true },
        }),
        (ProofBinary::Less, Some(bound), None) => Some(Comparison {
            subject: right,
            bound,
            kind: BoundKind::Lower { inclusive: false },
        }),
        (ProofBinary::LessEqual, Some(bound), None) => Some(Comparison {
            subject: right,
            bound,
            kind: BoundKind::Lower { inclusive: true },
        }),
        _ => None,
    }
}

fn numeric_bound(term: &ProofTerm) -> Option<NumericBound> {
    match term {
        ProofTerm::Constant(ProofConstant::Int(value)) => Some(NumericBound::Int(*value)),
        ProofTerm::Constant(ProofConstant::Float(bits)) => {
            let value = f64::from_bits(*bits);
            value.is_finite().then_some(NumericBound::Float(value))
        }
        _ => None,
    }
}

fn comparison_implies(known: &ProofTerm, goal: &ProofTerm) -> bool {
    let (Some(known), Some(goal)) = (comparison(known), comparison(goal)) else {
        return false;
    };
    if known.subject != goal.subject {
        return false;
    }
    let Some(ordering) = known.bound.compare(goal.bound) else {
        return false;
    };
    match (known.kind, goal.kind) {
        (BoundKind::Equal, BoundKind::Equal) => ordering == Ordering::Equal,
        (BoundKind::Equal, BoundKind::Lower { inclusive }) => {
            ordering == Ordering::Greater || (ordering == Ordering::Equal && inclusive)
        }
        (BoundKind::Equal, BoundKind::Upper { inclusive }) => {
            ordering == Ordering::Less || (ordering == Ordering::Equal && inclusive)
        }
        (
            BoundKind::Lower {
                inclusive: known_inclusive,
            },
            BoundKind::Lower {
                inclusive: goal_inclusive,
            },
        ) => {
            ordering == Ordering::Greater
                || (ordering == Ordering::Equal && (!known_inclusive || goal_inclusive))
        }
        (
            BoundKind::Upper {
                inclusive: known_inclusive,
            },
            BoundKind::Upper {
                inclusive: goal_inclusive,
            },
        ) => {
            ordering == Ordering::Less
                || (ordering == Ordering::Equal && (!known_inclusive || goal_inclusive))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(index: u32) -> ProofTerm {
        ProofTerm::Place(ProofPlace {
            root: ProofRoot::Local(index),
            fields: Vec::new(),
        })
    }

    #[test]
    fn constants_and_bound_implications_are_soundly_proven() {
        let folded = ProofTerm::binary(
            ProofBinary::LessEqual,
            ProofTerm::float(0.0),
            ProofTerm::binary(
                ProofBinary::Add,
                ProofTerm::float(5.0),
                ProofTerm::float(2.0),
            ),
        );
        assert_eq!(ProofFacts::default().prove(&folded), ProofResult::Proven);

        let value = local(0);
        let stronger = ProofTerm::binary(ProofBinary::Less, ProofTerm::float(0.0), value.clone());
        let weaker = ProofTerm::binary(ProofBinary::LessEqual, ProofTerm::float(0.0), value);
        let mut facts = ProofFacts::default();
        facts.assume(stronger, true);
        assert_eq!(facts.prove(&weaker), ProofResult::Proven);
    }

    #[test]
    fn false_float_comparison_does_not_invent_a_nan_unsafe_opposite() {
        let value = local(0);
        let lower = ProofTerm::binary(ProofBinary::LessEqual, ProofTerm::float(0.0), value);
        let mut facts = ProofFacts::default();
        facts.assume(lower.clone(), false);
        assert_eq!(facts.prove(&lower), ProofResult::Disproven);
    }
}
