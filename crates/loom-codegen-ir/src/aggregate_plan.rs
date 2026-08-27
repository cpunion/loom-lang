use std::collections::{BTreeMap, BTreeSet};

use loom_mir::{self as mir, Type, TypeId};

use crate::{BuildError, ProgramBuilder, ValueTypeId};

pub(crate) const fn is_direct_scalar(ty: &Type) -> bool {
    matches!(ty, Type::Unit | Type::Bool | Type::Int | Type::Float)
}

/// Upper bound for every semantic-type tree copied into the direct aggregate
/// plan. This is deliberately independent from the representation-node budget:
/// a generic schema can have few payload fields while substituting a very large
/// type tree into each field.
pub(crate) const DIRECT_AGGREGATE_MAX_TYPE_NODES: usize =
    crate::repr::DIRECT_PRODUCT_MAX_STRUCTURAL_NODES;

pub(crate) fn closed_record_fields<'program>(
    program: &'program mir::Program,
    ty: &Type,
) -> Option<&'program [mir::FieldDef]> {
    let Type::Nominal(id, arguments) = ty else {
        return None;
    };
    if !arguments.is_empty() {
        return None;
    }
    let definition = program.type_def(*id)?;
    if definition.type_parameters != 0 {
        return None;
    }
    let mir::TypeDefKind::Record { fields, invariant } = &definition.kind else {
        return None;
    };
    invariant.is_none().then_some(fields)
}

/// Resolves one fully concrete enum instantiation into ordered, substituted
/// payload types. The recursive direct-value classifier rejects managed,
/// erroneous, open, and cyclic payload graphs.
pub(crate) fn closed_enum_variants(
    program: &mir::Program,
    ty: &Type,
) -> Option<Box<[Box<[Type]>]>> {
    let Type::Nominal(id, arguments) = ty else {
        return None;
    };
    let definition = program.type_def(*id)?;
    if usize::try_from(definition.type_parameters).ok()? != arguments.len() {
        return None;
    }
    let mir::TypeDefKind::Enum { variants } = &definition.kind else {
        return None;
    };
    if variants.is_empty() {
        return None;
    }

    // Bound the borrowed declaration shape before allocating the result or
    // cloning/substituting even one payload type. In particular, a very wide
    // tag-only enum must not allocate one empty Box per variant merely to be
    // rejected by AggregateShape::structural_cost afterwards.
    variants.iter().try_fold(
        DIRECT_AGGREGATE_MAX_TYPE_NODES
            .checked_sub(1)?
            .checked_sub(variants.len())?,
        |remaining, variant| remaining.checked_sub(variant.payload.len()),
    )?;
    if arguments
        .iter()
        .any(|argument| concrete_type_node_count(argument).is_none())
    {
        return None;
    }

    let mut type_nodes = 0_usize;
    let mut planned = Vec::with_capacity(variants.len());
    for (index, variant) in variants.iter().enumerate() {
        if variant.id.0 as usize != index {
            return None;
        }
        let mut payloads = Vec::new();
        for payload in &variant.payload {
            let remaining = DIRECT_AGGREGATE_MAX_TYPE_NODES.checked_sub(type_nodes)?;
            let cost = substituted_type_node_count(payload, arguments, remaining)?;
            type_nodes = type_nodes.checked_add(cost)?;
            payloads.push(substitute_type(payload, arguments, 1)?);
        }
        planned.push(payloads.into_boxed_slice());
    }
    Some(planned.into_boxed_slice())
}

fn concrete_type_node_count(root: &Type) -> Option<usize> {
    let mut pending = vec![root];
    let mut nodes = 0_usize;
    while let Some(ty) = pending.pop() {
        nodes = nodes.checked_add(1)?;
        if nodes > DIRECT_AGGREGATE_MAX_TYPE_NODES {
            return None;
        }
        match ty {
            Type::Tuple(elements) => push_bounded(&mut pending, elements, nodes)?,
            Type::List(element) | Type::Task(element) | Type::TaskOutcome(element) => {
                push_bounded(&mut pending, std::slice::from_ref(element.as_ref()), nodes)?;
            }
            Type::Nominal(_, arguments) => push_bounded(&mut pending, arguments, nodes)?,
            Type::View { bindings, .. } => {
                if nodes
                    .checked_add(pending.len())?
                    .checked_add(bindings.len())?
                    > DIRECT_AGGREGATE_MAX_TYPE_NODES
                {
                    return None;
                }
                pending.extend(bindings.values());
            }
            Type::Parameter(_) | Type::AssociatedProjection { .. } | Type::Error => return None,
            Type::Never | Type::Unit | Type::Bool | Type::Int | Type::Float | Type::Text => {}
        }
    }
    Some(nodes)
}

fn push_bounded<'a>(
    pending: &mut Vec<&'a Type>,
    children: &'a [Type],
    visited: usize,
) -> Option<()> {
    if visited
        .checked_add(pending.len())?
        .checked_add(children.len())?
        > DIRECT_AGGREGATE_MAX_TYPE_NODES
    {
        return None;
    }
    pending.extend(children);
    Some(())
}

fn substituted_type_node_count(ty: &Type, arguments: &[Type], limit: usize) -> Option<usize> {
    let mut pending = vec![ty];
    let mut nodes = 0_usize;
    while let Some(current) = pending.pop() {
        if let Type::Parameter(index) = current {
            let argument = arguments.get(*index as usize)?;
            if nodes.checked_add(pending.len())? >= limit {
                return None;
            }
            pending.push(argument);
            continue;
        }
        nodes = nodes.checked_add(1)?;
        if nodes > limit {
            return None;
        }
        match current {
            Type::Tuple(elements) | Type::Nominal(_, elements) => {
                for child in elements {
                    push_substituted_bounded(&mut pending, child, nodes, limit)?;
                }
            }
            Type::List(element) | Type::Task(element) | Type::TaskOutcome(element) => {
                push_substituted_bounded(&mut pending, element, nodes, limit)?;
            }
            Type::View { bindings, .. } => {
                for child in bindings.values() {
                    push_substituted_bounded(&mut pending, child, nodes, limit)?;
                }
            }
            Type::Parameter(_) => unreachable!("parameters are handled before charging a node"),
            Type::Never
            | Type::Unit
            | Type::Bool
            | Type::Int
            | Type::Float
            | Type::Text
            | Type::AssociatedProjection { .. }
            | Type::Error => {}
        }
    }
    Some(nodes)
}

fn push_substituted_bounded<'a>(
    pending: &mut Vec<&'a Type>,
    child: &'a Type,
    visited: usize,
    limit: usize,
) -> Option<()> {
    if visited.checked_add(pending.len())?.checked_add(1)? > limit {
        return None;
    }
    pending.push(child);
    Some(())
}

fn substitute_type(ty: &Type, arguments: &[Type], depth: usize) -> Option<Type> {
    if depth > DIRECT_AGGREGATE_MAX_TYPE_NODES {
        return None;
    }
    Some(match ty {
        Type::Parameter(index) => arguments.get(*index as usize)?.clone(),
        Type::Tuple(elements) => Type::Tuple(
            elements
                .iter()
                .map(|element| substitute_type(element, arguments, depth.saturating_add(1)))
                .collect::<Option<Vec<_>>>()?,
        ),
        Type::List(element) => Type::List(Box::new(substitute_type(
            element,
            arguments,
            depth.saturating_add(1),
        )?)),
        Type::Nominal(id, nested) => Type::Nominal(
            *id,
            nested
                .iter()
                .map(|nested| substitute_type(nested, arguments, depth.saturating_add(1)))
                .collect::<Option<Vec<_>>>()?,
        ),
        Type::Task(output) => Type::Task(Box::new(substitute_type(
            output,
            arguments,
            depth.saturating_add(1),
        )?)),
        Type::TaskOutcome(output) => Type::TaskOutcome(Box::new(substitute_type(
            output,
            arguments,
            depth.saturating_add(1),
        )?)),
        Type::View {
            mutable,
            concept,
            bindings,
        } => Type::View {
            mutable: *mutable,
            concept: *concept,
            bindings: bindings
                .iter()
                .map(|(name, ty)| {
                    Some((
                        name.clone(),
                        substitute_type(ty, arguments, depth.saturating_add(1))?,
                    ))
                })
                .collect::<Option<_>>()?,
        },
        Type::Never => Type::Never,
        Type::Unit => Type::Unit,
        Type::Bool => Type::Bool,
        Type::Int => Type::Int,
        Type::Float => Type::Float,
        Type::Text => Type::Text,
        Type::AssociatedProjection {
            witness,
            associated,
        } => Type::AssociatedProjection {
            witness: *witness,
            associated: associated.clone(),
        },
        Type::Error => Type::Error,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum AggregateShape {
    Product(Box<[Type]>),
    InvariantProduct(Box<[Type]>),
    Transparent(Type),
    Sum(Box<[Box<[Type]>]>),
}

impl AggregateShape {
    fn dependencies(&self) -> Box<dyn Iterator<Item = &Type> + '_> {
        match self {
            Self::Product(fields) | Self::InvariantProduct(fields) => Box::new(fields.iter()),
            Self::Transparent(base) => Box::new(std::iter::once(base)),
            Self::Sum(variants) => Box::new(variants.iter().flat_map(|variant| variant.iter())),
        }
    }

    fn structural_cost(&self) -> Option<usize> {
        match self {
            Self::Product(fields) | Self::InvariantProduct(fields) => {
                1_usize.checked_add(fields.len())
            }
            Self::Transparent(_) => Some(2),
            Self::Sum(variants) => variants
                .iter()
                .try_fold(1_usize.checked_add(variants.len())?, |nodes, variant| {
                    nodes.checked_add(variant.len())
                }),
        }
    }
}

fn direct_aggregate_shape(program: &mir::Program, ty: &Type) -> Option<AggregateShape> {
    concrete_type_node_count(ty)?;
    match ty {
        Type::Tuple(elements) => Some(AggregateShape::Product(elements.clone().into_boxed_slice())),
        Type::Nominal(id, arguments) => {
            let definition = program.type_def(*id)?;
            match &definition.kind {
                mir::TypeDefKind::Enum { .. } => {
                    closed_enum_variants(program, ty).map(AggregateShape::Sum)
                }
                mir::TypeDefKind::Record { fields, invariant }
                    if arguments.is_empty() && definition.type_parameters == 0 =>
                {
                    let mut nodes = 0_usize;
                    for field in fields {
                        nodes = nodes.checked_add(concrete_type_node_count(&field.ty)?)?;
                        if nodes > DIRECT_AGGREGATE_MAX_TYPE_NODES {
                            return None;
                        }
                    }
                    let fields = fields
                        .iter()
                        .map(|field| field.ty.clone())
                        .collect::<Vec<_>>()
                        .into_boxed_slice();
                    Some(if invariant.is_some() {
                        AggregateShape::InvariantProduct(fields)
                    } else {
                        AggregateShape::Product(fields)
                    })
                }
                mir::TypeDefKind::Refined { base, .. }
                    if arguments.is_empty() && definition.type_parameters == 0 =>
                {
                    concrete_type_node_count(base)?;
                    Some(AggregateShape::Transparent(base.clone()))
                }
                mir::TypeDefKind::Record { .. } | mir::TypeDefKind::Refined { .. } => None,
            }
        }
        Type::Never
        | Type::Unit
        | Type::Bool
        | Type::Int
        | Type::Float
        | Type::Text
        | Type::List(_)
        | Type::Parameter(_)
        | Type::AssociatedProjection { .. }
        | Type::Task(_)
        | Type::TaskOutcome(_)
        | Type::View { .. }
        | Type::Error => None,
    }
}

/// Detects a by-value nominal cycle from borrowed declaration schemas. This
/// runs before any concrete type is cloned or a generic payload is
/// substituted, so non-regular recursions such as `Spiral[(T, T)]` cannot
/// grow their argument tree while the planner is still discovering the cycle.
fn has_by_value_nominal_cycle(program: &mir::Program, root: TypeId) -> Option<bool> {
    let mut pending = vec![(root, false)];
    let mut visiting = BTreeSet::new();
    let mut complete = BTreeSet::new();
    let mut inspected = 0_usize;
    while let Some((id, exiting)) = pending.pop() {
        if exiting {
            visiting.remove(&id);
            complete.insert(id);
            continue;
        }
        if complete.contains(&id) {
            continue;
        }
        if !visiting.insert(id) {
            return Some(true);
        }
        inspected = inspected.checked_add(1)?;
        if inspected > DIRECT_AGGREGATE_MAX_TYPE_NODES {
            return None;
        }
        let definition = program.type_def(id)?;
        let root_count = match &definition.kind {
            mir::TypeDefKind::Record { fields, .. } => fields.len(),
            mir::TypeDefKind::Enum { variants } => {
                variants.iter().try_fold(0_usize, |count, variant| {
                    count.checked_add(variant.payload.len())
                })?
            }
            mir::TypeDefKind::Refined { .. } => 1,
        };
        if inspected.checked_add(root_count)? > DIRECT_AGGREGATE_MAX_TYPE_NODES {
            return None;
        }
        let mut types = Vec::with_capacity(root_count);
        match &definition.kind {
            mir::TypeDefKind::Record { fields, .. } => {
                types.extend(fields.iter().map(|field| &field.ty));
            }
            mir::TypeDefKind::Enum { variants } => {
                types.extend(variants.iter().flat_map(|variant| &variant.payload));
            }
            mir::TypeDefKind::Refined { base, .. } => types.push(base),
        }
        let mut dependencies = BTreeSet::new();
        while let Some(ty) = types.pop() {
            inspected = inspected.checked_add(1)?;
            if inspected > DIRECT_AGGREGATE_MAX_TYPE_NODES {
                return None;
            }
            match ty {
                Type::Tuple(elements) => {
                    push_schema_types(&mut types, elements, inspected)?;
                }
                Type::Nominal(dependency, arguments) => {
                    dependencies.insert(*dependency);
                    push_schema_types(&mut types, arguments, inspected)?;
                }
                Type::Parameter(_)
                | Type::Never
                | Type::Unit
                | Type::Bool
                | Type::Int
                | Type::Float
                | Type::Text
                | Type::List(_)
                | Type::AssociatedProjection { .. }
                | Type::Task(_)
                | Type::TaskOutcome(_)
                | Type::View { .. }
                | Type::Error => {}
            }
        }
        pending.push((id, true));
        pending.extend(
            dependencies
                .into_iter()
                .rev()
                .map(|dependency| (dependency, false)),
        );
    }
    Some(false)
}

fn push_schema_types<'a>(
    pending: &mut Vec<&'a Type>,
    children: &'a [Type],
    inspected: usize,
) -> Option<()> {
    if inspected
        .checked_add(pending.len())?
        .checked_add(children.len())?
        > DIRECT_AGGREGATE_MAX_TYPE_NODES
    {
        return None;
    }
    pending.extend(children);
    Some(())
}

/// Classifies concrete immutable products and closed sums without constructing
/// LCIR. Every root is checked against the same expanded depth/node budgets as
/// independent validation, including mixed product/sum cycles.
pub(crate) struct AggregatePlanner<'program> {
    program: &'program mir::Program,
    planned: BTreeMap<Type, AggregateShape>,
    rejected_roots: BTreeSet<Type>,
    acyclic_nominals: BTreeSet<TypeId>,
    rejected_nominals: BTreeSet<TypeId>,
}

impl<'program> AggregatePlanner<'program> {
    pub(crate) fn new(program: &'program mir::Program) -> Self {
        Self {
            program,
            planned: BTreeMap::new(),
            rejected_roots: BTreeSet::new(),
            acyclic_nominals: BTreeSet::new(),
            rejected_nominals: BTreeSet::new(),
        }
    }

    fn supports_nominal_schema(&mut self, id: TypeId) -> bool {
        if self.acyclic_nominals.contains(&id) {
            return true;
        }
        if self.rejected_nominals.contains(&id) {
            return false;
        }
        if has_by_value_nominal_cycle(self.program, id) == Some(false) {
            self.acyclic_nominals.insert(id);
            true
        } else {
            self.rejected_nominals.insert(id);
            false
        }
    }

    pub(crate) fn supports_value_type(&mut self, ty: &Type) -> bool {
        if is_direct_scalar(ty) {
            return true;
        }
        if self.planned.contains_key(ty) {
            return true;
        }
        if self.rejected_roots.contains(ty) {
            return false;
        }

        // Do not clone an attacker-controlled semantic tree into planner maps
        // until its complete structure is known to fit the direct-type budget.
        if concrete_type_node_count(ty).is_none() {
            return false;
        }

        // Exit frames keep cycle detection path-local. Repeated children are
        // expanded because validator budgets count occurrences, not identities.
        let mut pending = vec![(ty.clone(), 1_usize, false)];
        let mut visiting = BTreeSet::new();
        let mut discovered = BTreeMap::new();
        let mut structural_nodes = 0_usize;
        let mut supported = true;
        while let Some((semantic, depth, exiting)) = pending.pop() {
            if exiting {
                visiting.remove(&semantic);
                continue;
            }
            if depth > crate::repr::DIRECT_PRODUCT_MAX_NESTING_DEPTH
                || !visiting.insert(semantic.clone())
            {
                supported = false;
                break;
            }
            if let Type::Nominal(id, _) = &semantic
                && !self.supports_nominal_schema(*id)
            {
                supported = false;
                break;
            }
            let Some(shape) = direct_aggregate_shape(self.program, &semantic) else {
                supported = false;
                break;
            };
            let Some(next_structural_nodes) = shape
                .structural_cost()
                .and_then(|cost| structural_nodes.checked_add(cost))
            else {
                supported = false;
                break;
            };
            if next_structural_nodes > crate::repr::DIRECT_PRODUCT_MAX_STRUCTURAL_NODES {
                supported = false;
                break;
            }
            structural_nodes = next_structural_nodes;
            let children = shape
                .dependencies()
                .filter(|field| !is_direct_scalar(field))
                .cloned()
                .collect::<Vec<_>>();
            discovered.entry(semantic.clone()).or_insert(shape);
            pending.push((semantic, depth, true));
            pending.extend(
                children
                    .into_iter()
                    .rev()
                    .map(|child| (child, depth.saturating_add(1), false)),
            );
        }

        if supported {
            self.planned.extend(discovered);
        } else {
            self.rejected_roots.insert(ty.clone());
        }
        supported
    }

    pub(crate) fn finish(self) -> AggregatePlan {
        AggregatePlan {
            entries: self.planned,
        }
    }
}

pub(crate) enum AggregateRegistrationError {
    Build(BuildError),
    Inconsistent(String),
}

/// A complete concrete direct-aggregate plan built before LCIR allocation.
pub(crate) struct AggregatePlan {
    entries: BTreeMap<Type, AggregateShape>,
}

impl AggregatePlan {
    pub(crate) fn register(
        self,
        builder: &mut ProgramBuilder,
    ) -> Result<(), AggregateRegistrationError> {
        let mut remaining_dependencies = BTreeMap::new();
        let mut dependents: BTreeMap<Type, Vec<Type>> = BTreeMap::new();
        let mut ready = BTreeSet::new();

        for (semantic, shape) in &self.entries {
            if let (Type::Tuple(elements), AggregateShape::Product(fields)) = (semantic, shape)
                && elements.as_slice() != fields.as_ref()
            {
                return Err(AggregateRegistrationError::Inconsistent(format!(
                    "tuple plan {semantic:?} does not match its element types"
                )));
            }
            if shape
                .dependencies()
                .any(|field| !is_direct_scalar(field) && !self.entries.contains_key(field))
            {
                return Err(AggregateRegistrationError::Inconsistent(format!(
                    "direct type {semantic:?} depends on an unplanned value type"
                )));
            }
            let dependencies = shape
                .dependencies()
                .filter(|field| self.entries.contains_key(*field))
                .cloned()
                .collect::<BTreeSet<_>>();
            for dependency in &dependencies {
                dependents
                    .entry(dependency.clone())
                    .or_default()
                    .push(semantic.clone());
            }
            if dependencies.is_empty() {
                ready.insert(semantic.clone());
            }
            remaining_dependencies.insert(semantic.clone(), dependencies.len());
        }

        let mut registered = 0_usize;
        while let Some(semantic) = ready.pop_first() {
            let shape = self.entries.get(&semantic).ok_or_else(|| {
                AggregateRegistrationError::Inconsistent(format!(
                    "ready direct type {semantic:?} disappeared from its plan"
                ))
            })?;
            let _: ValueTypeId = match (&semantic, shape) {
                (Type::Tuple(elements), AggregateShape::Product(_)) => {
                    builder.add_tuple_type(elements)
                }
                (Type::Nominal(_, arguments), AggregateShape::Product(fields))
                    if arguments.is_empty() =>
                {
                    builder.add_pod_record_type(semantic.clone(), fields)
                }
                (Type::Nominal(_, arguments), AggregateShape::InvariantProduct(fields))
                    if arguments.is_empty() =>
                {
                    builder.add_invariant_record_type(semantic.clone(), fields)
                }
                (Type::Nominal(_, arguments), AggregateShape::Transparent(base))
                    if arguments.is_empty() =>
                {
                    builder.add_transparent_type(semantic.clone(), base)
                }
                (Type::Nominal(_, _), AggregateShape::Sum(variants)) => {
                    builder.add_sum_type(semantic.clone(), variants)
                }
                _ => {
                    return Err(AggregateRegistrationError::Inconsistent(format!(
                        "direct-type plan contains invalid semantic type {semantic:?}"
                    )));
                }
            }
            .map_err(AggregateRegistrationError::Build)?;
            registered = registered.saturating_add(1);

            for dependent in dependents.get(&semantic).into_iter().flatten() {
                let remaining = remaining_dependencies.get_mut(dependent).ok_or_else(|| {
                    AggregateRegistrationError::Inconsistent(format!(
                        "dependent aggregate {dependent:?} disappeared from its plan"
                    ))
                })?;
                *remaining = remaining.saturating_sub(1);
                if *remaining == 0 {
                    ready.insert(dependent.clone());
                }
            }
        }

        if registered != self.entries.len() {
            return Err(AggregateRegistrationError::Inconsistent(
                "direct aggregate plan contains a cycle".to_owned(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use loom_core::Span;
    use loom_mir::{Program, TypeDef, TypeDefKind, VariantDef, VariantId};

    use super::*;

    #[test]
    fn non_regular_generic_recursion_is_rejected_before_substitution_growth() {
        let spiral = TypeId(0);
        let program = Program {
            types: vec![TypeDef {
                id: spiral,
                name: "Spiral".into(),
                span: Span::default(),
                type_parameters: 1,
                kind: TypeDefKind::Enum {
                    variants: vec![
                        VariantDef {
                            id: VariantId(0),
                            name: "Done".into(),
                            payload: vec![Type::Parameter(0)],
                            span: Span::default(),
                        },
                        VariantDef {
                            id: VariantId(1),
                            name: "Next".into(),
                            payload: vec![Type::Nominal(
                                spiral,
                                vec![Type::Tuple(vec![Type::Parameter(0), Type::Parameter(0)])],
                            )],
                            span: Span::default(),
                        },
                    ],
                },
            }],
            ..Program::default()
        };
        let root = Type::Nominal(spiral, vec![Type::Int]);

        let mut planner = AggregatePlanner::new(&program);
        assert!(
            !planner.supports_value_type(&Type::Tuple(vec![root.clone(), Type::Int])),
            "a tuple root must reject a nested non-regular nominal before substitution growth"
        );
        assert!(
            !planner.supports_value_type(&root),
            "a by-value nominal recursion must be rejected by identity before its generic argument doubles repeatedly"
        );
        assert!(
            !planner.supports_value_type(&root),
            "the rejected root must remain an atomic cached fallback"
        );
    }

    #[test]
    fn oversized_semantic_types_are_rejected_before_the_first_planner_clone() {
        let oversized = Type::Tuple(
            std::iter::repeat_n(Type::Int, DIRECT_AGGREGATE_MAX_TYPE_NODES).collect::<Vec<_>>(),
        );
        let program = Program::default();
        let mut planner = AggregatePlanner::new(&program);
        assert!(!planner.supports_value_type(&oversized));
    }

    #[test]
    fn wide_tag_only_sum_is_rejected_before_variant_allocation() {
        let wide = TypeId(0);
        let variants = (0..DIRECT_AGGREGATE_MAX_TYPE_NODES)
            .map(|index| VariantDef {
                id: VariantId(u32::try_from(index).expect("bounded variant id")),
                name: format!("V{index}"),
                payload: Vec::new(),
                span: Span::default(),
            })
            .collect();
        let program = Program {
            types: vec![TypeDef {
                id: wide,
                name: "Wide".into(),
                span: Span::default(),
                type_parameters: 0,
                kind: TypeDefKind::Enum { variants },
            }],
            ..Program::default()
        };
        let root = Type::Nominal(wide, Vec::new());

        assert!(closed_enum_variants(&program, &root).is_none());
        let mut planner = AggregatePlanner::new(&program);
        assert!(!planner.supports_value_type(&root));
        assert!(!planner.supports_value_type(&root));
    }

    #[test]
    fn repeated_acyclic_generic_nominal_instantiations_are_direct() {
        let option = TypeId(0);
        let program = Program {
            types: vec![TypeDef {
                id: option,
                name: "Option".into(),
                span: Span::default(),
                type_parameters: 1,
                kind: TypeDefKind::Enum {
                    variants: vec![
                        VariantDef {
                            id: VariantId(0),
                            name: "None".into(),
                            payload: Vec::new(),
                            span: Span::default(),
                        },
                        VariantDef {
                            id: VariantId(1),
                            name: "Some".into(),
                            payload: vec![Type::Parameter(0)],
                            span: Span::default(),
                        },
                    ],
                },
            }],
            ..Program::default()
        };
        let inner = Type::Nominal(option, vec![Type::Int]);
        let outer = Type::Nominal(option, vec![inner]);

        let mut planner = AggregatePlanner::new(&program);
        assert!(planner.supports_value_type(&outer));
        let mut builder = ProgramBuilder::new(crate::TargetLayout::new(64).expect("target"));
        planner
            .finish()
            .register(&mut builder)
            .unwrap_or_else(|_| panic!("nested Option plan must register"));
        assert!(builder.type_id(&outer).is_some());
    }
}
