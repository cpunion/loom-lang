use std::collections::BTreeMap;

use loom_mir::{self as mir, LocalId, Pattern, Type, TypeId, VariantId};

use crate::aggregate_plan::closed_enum_variants;

pub(crate) const DIRECT_MATCH_MAX_PATTERN_NODES: usize = 512;
pub(crate) const DIRECT_MATCH_MAX_DECISION_NODES: usize = 512;
pub(crate) const DIRECT_MATCH_MAX_VALUES: usize = 512;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct MatchNodeId(usize);

impl MatchNodeId {
    pub(crate) const fn index(self) -> usize {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct MatchValueId(usize);

impl MatchValueId {
    pub(crate) const fn index(self) -> usize {
        self.0
    }
}

#[derive(Clone, Debug)]
pub(crate) enum MatchNode {
    Arm {
        arm: usize,
        captures: Box<[(LocalId, MatchValueId)]>,
    },
    Constant {
        value: MatchValueId,
        constant: mir::Constant,
        equal: MatchNodeId,
        not_equal: MatchNodeId,
    },
    Sum {
        value: MatchValueId,
        cases: Box<[MatchCase]>,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct MatchCase {
    pub(crate) variant: u32,
    pub(crate) payload: Box<[MatchValueId]>,
    pub(crate) next: MatchNodeId,
}

#[derive(Clone, Debug)]
pub(crate) struct MatchPlan {
    root: MatchNodeId,
    values: Box<[Type]>,
    nodes: Box<[MatchNode]>,
}

impl MatchPlan {
    pub(crate) const fn root(&self) -> MatchNodeId {
        self.root
    }

    pub(crate) fn value_type(&self, value: MatchValueId) -> Option<&Type> {
        self.values.get(value.index())
    }

    pub(crate) fn node(&self, node: MatchNodeId) -> Option<&MatchNode> {
        self.nodes.get(node.index())
    }

    pub(crate) const fn value_count(&self) -> usize {
        self.values.len()
    }
}

#[derive(Clone, Debug)]
enum PlannedPattern {
    Wildcard,
    Binding(LocalId),
    Constant(mir::Constant),
    Variant {
        ty: TypeId,
        variant: VariantId,
        payload: Vec<PlannedPattern>,
    },
}

#[derive(Clone)]
struct Row {
    arm: usize,
    patterns: Vec<PlannedPattern>,
    captures: Vec<(LocalId, MatchValueId)>,
}

struct Planner<'program> {
    program: &'program mir::Program,
    values: Vec<Type>,
    nodes: Vec<MatchNode>,
    arm_nodes: BTreeMap<(usize, Vec<(LocalId, MatchValueId)>), MatchNodeId>,
}

pub(crate) fn plan_match(
    program: &mir::Program,
    scrutinee: &Type,
    arms: &[mir::MatchArm],
) -> Option<MatchPlan> {
    let pattern_nodes = arms.iter().try_fold(0_usize, |total, arm| {
        total.checked_add(pattern_node_count(&arm.pattern)?)
    })?;
    if pattern_nodes > DIRECT_MATCH_MAX_PATTERN_NODES || arms.is_empty() {
        return None;
    }

    let mut rows = Vec::with_capacity(arms.len());
    for (arm_index, arm) in arms.iter().enumerate() {
        let mut bindings = arm.bindings.iter().copied();
        let pattern = annotate_pattern(&arm.pattern, &mut bindings)?;
        if bindings.next().is_some() {
            return None;
        }
        rows.push(Row {
            arm: arm_index,
            patterns: vec![pattern],
            captures: Vec::new(),
        });
    }

    let mut planner = Planner {
        program,
        values: vec![scrutinee.clone()],
        nodes: Vec::new(),
        arm_nodes: BTreeMap::new(),
    };
    let root = planner.compile(rows, vec![MatchValueId(0)])?;
    Some(MatchPlan {
        root,
        values: planner.values.into_boxed_slice(),
        nodes: planner.nodes.into_boxed_slice(),
    })
}

fn pattern_node_count(pattern: &Pattern) -> Option<usize> {
    match pattern {
        Pattern::Wildcard | Pattern::Binding | Pattern::Constant(_) => Some(1),
        Pattern::Variant { payload, .. } => payload.iter().try_fold(1_usize, |total, child| {
            total.checked_add(pattern_node_count(child)?)
        }),
    }
}

fn annotate_pattern(
    pattern: &Pattern,
    bindings: &mut impl Iterator<Item = LocalId>,
) -> Option<PlannedPattern> {
    Some(match pattern {
        Pattern::Wildcard => PlannedPattern::Wildcard,
        Pattern::Binding => PlannedPattern::Binding(bindings.next()?),
        Pattern::Constant(constant) => PlannedPattern::Constant(constant.clone()),
        Pattern::Variant {
            ty,
            variant,
            payload,
        } => PlannedPattern::Variant {
            ty: *ty,
            variant: *variant,
            payload: payload
                .iter()
                .map(|child| annotate_pattern(child, bindings))
                .collect::<Option<Vec<_>>>()?,
        },
    })
}

impl Planner<'_> {
    #[expect(
        clippy::too_many_lines,
        reason = "the bounded matrix-specialization cases stay together so source order and capture propagation remain auditable"
    )]
    fn compile(&mut self, mut rows: Vec<Row>, columns: Vec<MatchValueId>) -> Option<MatchNodeId> {
        if self.nodes.len() >= DIRECT_MATCH_MAX_DECISION_NODES || rows.is_empty() {
            return None;
        }
        for row in &mut rows {
            if row.patterns.len() != columns.len() {
                return None;
            }
            for (pattern, value) in row.patterns.iter_mut().zip(&columns) {
                if let PlannedPattern::Binding(local) = pattern {
                    row.captures.push((*local, *value));
                    *pattern = PlannedPattern::Wildcard;
                }
            }
        }

        let first = rows.first()?;
        let Some(column) = first
            .patterns
            .iter()
            .position(|pattern| !matches!(pattern, PlannedPattern::Wildcard))
        else {
            return self.arm(first.arm, first.captures.clone());
        };
        let value = *columns.get(column)?;
        let ty = self.values.get(value.index())?.clone();
        match first.patterns.get(column)?.clone() {
            PlannedPattern::Constant(mir::Constant::Unit) if ty == Type::Unit => {
                rows.first_mut()?.patterns[column] = PlannedPattern::Wildcard;
                self.compile(rows, columns)
            }
            PlannedPattern::Constant(constant) => {
                if !constant_matches_type(&constant, &ty) {
                    return None;
                }
                let mut equal_rows = Vec::new();
                let mut not_equal_rows = Vec::new();
                for mut row in rows {
                    match row.patterns.get(column)? {
                        PlannedPattern::Wildcard => {
                            equal_rows.push(row.clone());
                            not_equal_rows.push(row);
                        }
                        PlannedPattern::Constant(candidate) => {
                            if same_constant(candidate, &constant) {
                                let mut equal = row;
                                equal.patterns[column] = PlannedPattern::Wildcard;
                                equal_rows.push(equal);
                            } else if matches!(
                                (&constant, candidate),
                                (
                                    mir::Constant::Bool(left),
                                    mir::Constant::Bool(right)
                                ) if left != right
                            ) {
                                // Bool has exactly two inhabitants. After the
                                // selected value failed one Bool constant, the
                                // opposite constant is irrefutable on that edge.
                                row.patterns[column] = PlannedPattern::Wildcard;
                                not_equal_rows.push(row);
                            } else {
                                not_equal_rows.push(row);
                            }
                        }
                        PlannedPattern::Binding(_) | PlannedPattern::Variant { .. } => return None,
                    }
                }
                let equal = self.compile(equal_rows, columns.clone())?;
                let not_equal = self.compile(not_equal_rows, columns)?;
                self.push_node(MatchNode::Constant {
                    value,
                    constant,
                    equal,
                    not_equal,
                })
            }
            PlannedPattern::Variant { .. } => {
                let Type::Nominal(type_id, _) = ty.clone() else {
                    return None;
                };
                let variants = closed_enum_variants(self.program, &ty)?;
                let mut cases = Vec::with_capacity(variants.len());
                for (variant_index, payload_types) in variants.iter().enumerate() {
                    let payload = payload_types
                        .iter()
                        .cloned()
                        .map(|payload| self.push_value(payload))
                        .collect::<Option<Vec<_>>>()?;
                    let mut specialized = Vec::new();
                    for mut row in rows.iter().cloned() {
                        match row.patterns.remove(column) {
                            PlannedPattern::Wildcard => {
                                row.patterns.splice(
                                    column..column,
                                    std::iter::repeat_n(
                                        PlannedPattern::Wildcard,
                                        payload_types.len(),
                                    ),
                                );
                                specialized.push(row);
                            }
                            PlannedPattern::Variant {
                                ty,
                                variant,
                                payload,
                            } => {
                                if ty != type_id {
                                    return None;
                                }
                                if variant.0 as usize == variant_index {
                                    if payload.len() != payload_types.len() {
                                        return None;
                                    }
                                    row.patterns.splice(column..column, payload);
                                    specialized.push(row);
                                }
                            }
                            PlannedPattern::Binding(_) | PlannedPattern::Constant(_) => {
                                return None;
                            }
                        }
                    }
                    let mut next_columns = columns.clone();
                    next_columns.remove(column);
                    next_columns.splice(column..column, payload.iter().copied());
                    let next = self.compile(specialized, next_columns)?;
                    cases.push(MatchCase {
                        variant: u32::try_from(variant_index).ok()?,
                        payload: payload.into_boxed_slice(),
                        next,
                    });
                }
                self.push_node(MatchNode::Sum {
                    value,
                    cases: cases.into_boxed_slice(),
                })
            }
            PlannedPattern::Wildcard | PlannedPattern::Binding(_) => None,
        }
    }

    fn push_value(&mut self, ty: Type) -> Option<MatchValueId> {
        if self.values.len() >= DIRECT_MATCH_MAX_VALUES {
            return None;
        }
        let id = MatchValueId(self.values.len());
        self.values.push(ty);
        Some(id)
    }

    fn arm(
        &mut self,
        arm: usize,
        mut captures: Vec<(LocalId, MatchValueId)>,
    ) -> Option<MatchNodeId> {
        captures.sort_unstable_by_key(|(local, _)| local.0);
        let key = (arm, captures.clone());
        if let Some(node) = self.arm_nodes.get(&key) {
            return Some(*node);
        }
        let node = self.push_node(MatchNode::Arm {
            arm,
            captures: captures.into_boxed_slice(),
        })?;
        self.arm_nodes.insert(key, node);
        Some(node)
    }

    fn push_node(&mut self, node: MatchNode) -> Option<MatchNodeId> {
        if self.nodes.len() >= DIRECT_MATCH_MAX_DECISION_NODES {
            return None;
        }
        let id = MatchNodeId(self.nodes.len());
        self.nodes.push(node);
        Some(id)
    }
}

fn constant_matches_type(constant: &mir::Constant, ty: &Type) -> bool {
    matches!(
        (constant, ty),
        (mir::Constant::Unit, Type::Unit)
            | (mir::Constant::Bool(_), Type::Bool)
            | (mir::Constant::Int(_), Type::Int)
            | (mir::Constant::Float(_), Type::Float)
    )
}

fn same_constant(left: &mir::Constant, right: &mir::Constant) -> bool {
    match (left, right) {
        (mir::Constant::Unit, mir::Constant::Unit) => true,
        (mir::Constant::Bool(left), mir::Constant::Bool(right)) => left == right,
        (mir::Constant::Int(left), mir::Constant::Int(right)) => left == right,
        (mir::Constant::Float(left), mir::Constant::Float(right)) => {
            left.to_bits() == right.to_bits()
        }
        (mir::Constant::Text(left), mir::Constant::Text(right)) => left == right,
        _ => false,
    }
}
