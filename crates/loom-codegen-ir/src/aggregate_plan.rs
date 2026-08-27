use std::collections::{BTreeMap, BTreeSet};

use loom_mir::{self as mir, Type, TypeId};

use crate::{BuildError, ProgramBuilder, ValueTypeId};

pub(crate) const fn is_direct_scalar(ty: &Type) -> bool {
    matches!(ty, Type::Unit | Type::Bool | Type::Int | Type::Float)
}

const fn is_direct_product_leaf(ty: &Type) -> bool {
    is_direct_scalar(ty) || matches!(ty, Type::Text)
}

/// Upper bound for every semantic-type tree copied into the direct aggregate
/// plan. This is deliberately independent from the representation-node budget:
/// a generic schema can have few payload fields while substituting a very large
/// type tree into each field.
pub(crate) const DIRECT_AGGREGATE_MAX_TYPE_NODES: usize =
    crate::repr::DIRECT_PRODUCT_MAX_STRUCTURAL_NODES;

/// Resolves one concrete record instantiation into ordered, substituted field
/// types. `TextMap` remains opaque because its raw field is a runtime handle,
/// not the source value's product representation.
pub(crate) fn concrete_record_fields(program: &mir::Program, ty: &Type) -> Option<Box<[Type]>> {
    let Type::Nominal(id, arguments) = ty else {
        return None;
    };
    if program.prelude.text_map == Some(*id) {
        return None;
    }
    let definition = program.type_def(*id)?;
    if usize::try_from(definition.type_parameters).ok()? != arguments.len() {
        return None;
    }
    let mir::TypeDefKind::Record { fields, invariant } = &definition.kind else {
        return None;
    };
    if invariant.is_some() {
        return None;
    }
    substitute_fields(fields, arguments)
}

/// Resolves fields for either a plain or invariant concrete record.
pub(crate) fn concrete_any_record_fields(program: &mir::Program, ty: &Type) -> Option<Box<[Type]>> {
    let Type::Nominal(id, arguments) = ty else {
        return None;
    };
    if program.prelude.text_map == Some(*id) {
        return None;
    }
    let definition = program.type_def(*id)?;
    if usize::try_from(definition.type_parameters).ok()? != arguments.len() {
        return None;
    }
    let mir::TypeDefKind::Record { fields, .. } = &definition.kind else {
        return None;
    };
    substitute_fields(fields, arguments)
}

/// Resolves the transparent payload of a concrete refined instantiation.
pub(crate) fn concrete_refined_base(program: &mir::Program, ty: &Type) -> Option<Type> {
    let Type::Nominal(id, arguments) = ty else {
        return None;
    };
    let definition = program.type_def(*id)?;
    if usize::try_from(definition.type_parameters).ok()? != arguments.len() {
        return None;
    }
    let mir::TypeDefKind::Refined { base, .. } = &definition.kind else {
        return None;
    };
    concrete_type_node_count(ty)?;
    substituted_type_node_count(base, arguments, DIRECT_AGGREGATE_MAX_TYPE_NODES)?;
    substitute_type(base, arguments, 1)
}

fn substitute_fields(fields: &[mir::FieldDef], arguments: &[Type]) -> Option<Box<[Type]>> {
    DIRECT_AGGREGATE_MAX_TYPE_NODES.checked_sub(fields.len())?;
    if arguments
        .iter()
        .any(|argument| concrete_type_node_count(argument).is_none())
    {
        return None;
    }
    let mut type_nodes = 0_usize;
    let mut concrete = Vec::with_capacity(fields.len());
    for field in fields {
        let remaining = DIRECT_AGGREGATE_MAX_TYPE_NODES.checked_sub(type_nodes)?;
        let cost = substituted_type_node_count(&field.ty, arguments, remaining)?;
        type_nodes = type_nodes.checked_add(cost)?;
        concrete.push(substitute_type(&field.ty, arguments, 1)?);
    }
    Some(concrete.into_boxed_slice())
}

/// Resolves one fully concrete enum instantiation into ordered, substituted
/// payload types. The recursive direct-value classifier rejects erroneous,
/// open, cyclic, and target-incompatible payload graphs.
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
    ManagedList(Type),
}

impl AggregateShape {
    fn dependencies(&self) -> Box<dyn Iterator<Item = &Type> + '_> {
        match self {
            Self::Product(fields) | Self::InvariantProduct(fields) => Box::new(fields.iter()),
            Self::Transparent(base) => Box::new(std::iter::once(base)),
            Self::Sum(variants) => Box::new(variants.iter().flat_map(|variant| variant.iter())),
            // A List is one pointer in its containing value. Its element graph
            // is checked independently, not as a by-value registration edge.
            Self::ManagedList(_) => Box::new(std::iter::empty()),
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
            Self::ManagedList(_) => Some(1),
        }
    }
}

fn direct_aggregate_shape(program: &mir::Program, ty: &Type) -> Option<AggregateShape> {
    concrete_type_node_count(ty)?;
    match ty {
        Type::Tuple(elements) => Some(AggregateShape::Product(elements.clone().into_boxed_slice())),
        Type::Nominal(id, _) => {
            let definition = program.type_def(*id)?;
            match &definition.kind {
                mir::TypeDefKind::Enum { .. } => {
                    closed_enum_variants(program, ty).map(AggregateShape::Sum)
                }
                mir::TypeDefKind::Record { invariant, .. } => {
                    let fields = concrete_any_record_fields(program, ty)?;
                    Some(if invariant.is_some() {
                        AggregateShape::InvariantProduct(fields)
                    } else {
                        AggregateShape::Product(fields)
                    })
                }
                mir::TypeDefKind::Refined { .. } => {
                    concrete_refined_base(program, ty).map(AggregateShape::Transparent)
                }
            }
        }
        Type::List(element) => Some(AggregateShape::ManagedList((**element).clone())),
        Type::Never
        | Type::Unit
        | Type::Bool
        | Type::Int
        | Type::Float
        | Type::Text
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
    supports_managed_text: bool,
    planned: BTreeMap<Type, AggregateShape>,
    rejected_roots: BTreeSet<Type>,
    acyclic_nominals: BTreeSet<TypeId>,
    rejected_nominals: BTreeSet<TypeId>,
    uses_text_aggregate_leaf: bool,
}

impl<'program> AggregatePlanner<'program> {
    pub(crate) fn new(program: &'program mir::Program, supports_managed_text: bool) -> Self {
        Self {
            program,
            supports_managed_text,
            planned: BTreeMap::new(),
            rejected_roots: BTreeSet::new(),
            acyclic_nominals: BTreeSet::new(),
            rejected_nominals: BTreeSet::new(),
            uses_text_aggregate_leaf: false,
        }
    }

    pub(crate) const fn uses_text_aggregate_leaf(&self) -> bool {
        self.uses_text_aggregate_leaf
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

    #[expect(
        clippy::too_many_lines,
        reason = "the bounded worklist keeps List cycle breaking and every direct aggregate eligibility rule in one auditable traversal"
    )]
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

        // List edges are physical cycle breakers, but their element values are
        // still checked as independent closed roots. Sharing discoveries across
        // those roots makes mutually recursive records connected only through
        // Lists finite without admitting a by-value cycle.
        let mut semantic_roots = vec![ty.clone()];
        let mut discovered = BTreeMap::new();
        let mut structural_nodes = 0_usize;
        let mut uses_text_aggregate_leaf = false;
        let mut supported = true;
        while let Some(root) = semantic_roots.pop() {
            if is_direct_scalar(&root)
                || self.planned.contains_key(&root)
                || discovered.contains_key(&root)
            {
                continue;
            }
            if root == Type::Text {
                if self.supports_managed_text {
                    uses_text_aggregate_leaf = true;
                } else {
                    supported = false;
                }
                continue;
            }
            if concrete_type_node_count(&root).is_none() {
                supported = false;
                break;
            }
            // Exit frames keep ordinary by-value cycle detection path-local.
            // List elements are queued above as fresh semantic roots instead.
            let mut pending = vec![(root, 1_usize, false, true)];
            let mut visiting = BTreeSet::new();
            while let Some((semantic, depth, exiting, managed_path)) = pending.pop() {
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
                if self.planned.contains_key(&semantic) || discovered.contains_key(&semantic) {
                    visiting.remove(&semantic);
                    continue;
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
                if let AggregateShape::ManagedList(element) = &shape {
                    if !managed_path || !self.supports_managed_text {
                        supported = false;
                        break;
                    }
                    if element == &Type::Text {
                        if !self.supports_managed_text {
                            supported = false;
                            break;
                        }
                        uses_text_aggregate_leaf = true;
                    } else if !is_direct_scalar(element) {
                        semantic_roots.push(element.clone());
                    }
                    discovered.entry(semantic.clone()).or_insert(shape);
                    visiting.remove(&semantic);
                    continue;
                }
                let child_managed_path = managed_path
                    && matches!(
                        shape,
                        AggregateShape::Product(_)
                            | AggregateShape::InvariantProduct(_)
                            | AggregateShape::Sum(_)
                    );
                let mut children = Vec::new();
                for field in shape.dependencies() {
                    if is_direct_scalar(field) {
                        continue;
                    }
                    if field == &Type::Text {
                        if !child_managed_path || !self.supports_managed_text {
                            supported = false;
                            break;
                        }
                        uses_text_aggregate_leaf = true;
                        continue;
                    }
                    children.push(field.clone());
                }
                if !supported {
                    break;
                }
                discovered.entry(semantic.clone()).or_insert(shape);
                pending.push((semantic, depth, true, managed_path));
                pending.extend(
                    children
                        .into_iter()
                        .rev()
                        .map(|child| (child, depth.saturating_add(1), false, child_managed_path)),
                );
            }
            if !supported {
                break;
            }
        }

        if supported {
            self.planned.extend(discovered);
            self.uses_text_aggregate_leaf |= uses_text_aggregate_leaf;
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
    #[allow(
        clippy::too_many_lines,
        reason = "registration preserves one explicit dependency order across List pointers, products, sums, and transparent aliases"
    )]
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
            if let AggregateShape::ManagedList(element) = shape
                && !is_direct_product_leaf(element)
                && !self.entries.contains_key(element)
            {
                return Err(AggregateRegistrationError::Inconsistent(format!(
                    "managed List {semantic:?} has an unplanned element value type"
                )));
            }
            if shape
                .dependencies()
                .any(|field| !is_direct_product_leaf(field) && !self.entries.contains_key(field))
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
                (Type::Nominal(_, _), AggregateShape::Product(fields)) => {
                    builder.add_pod_record_type(semantic.clone(), fields)
                }
                (Type::Nominal(_, _), AggregateShape::InvariantProduct(fields)) => {
                    builder.add_invariant_record_type(semantic.clone(), fields)
                }
                (Type::Nominal(_, _), AggregateShape::Transparent(base)) => {
                    builder.add_transparent_type(semantic.clone(), base)
                }
                (Type::Nominal(_, _), AggregateShape::Sum(variants)) => {
                    builder.add_sum_type(semantic.clone(), variants)
                }
                (Type::List(element), AggregateShape::ManagedList(planned))
                    if element.as_ref() == planned =>
                {
                    builder.add_managed_list_type(semantic.clone())
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
    use loom_mir::{
        Constant, Contract, ContractExpr, ContractExprKind, FieldDef, PreludeIds, Program, TypeDef,
        TypeDefKind, VariantDef, VariantId,
    };

    use super::*;

    #[test]
    fn managed_list_breaks_by_value_recursion_but_checks_its_element_graph() {
        let node = TypeId(0);
        let node_type = Type::Nominal(node, Vec::new());
        let list_type = Type::List(Box::new(node_type.clone()));
        let program = Program {
            types: vec![TypeDef {
                id: node,
                name: "Node".into(),
                span: Span::default(),
                type_parameters: 0,
                kind: TypeDefKind::Record {
                    fields: vec![loom_mir::FieldDef {
                        name: "children".into(),
                        ty: list_type.clone(),
                        span: Span::default(),
                    }],
                    invariant: None,
                },
            }],
            ..Program::default()
        };
        let mut planner = AggregatePlanner::new(&program, true);
        assert!(planner.supports_value_type(&node_type));
        assert!(planner.supports_value_type(&list_type));
        assert!(
            !planner.supports_value_type(&Type::List(Box::new(Type::Task(Box::new(Type::Int),))))
        );

        let mut builder = ProgramBuilder::new(crate::TargetLayout::new(64).expect("target"));
        planner
            .finish()
            .register(&mut builder)
            .unwrap_or_else(|_| panic!("register recursive List-broken plan"));
        assert!(builder.type_id(&node_type).is_some());
        assert!(builder.type_id(&list_type).is_some());

        let mut unsupported_target = AggregatePlanner::new(&program, false);
        assert!(!unsupported_target.supports_value_type(&list_type));
    }

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

        let mut planner = AggregatePlanner::new(&program, true);
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
        let mut planner = AggregatePlanner::new(&program, true);
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
        let mut planner = AggregatePlanner::new(&program, true);
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

        let mut planner = AggregatePlanner::new(&program, true);
        assert!(planner.supports_value_type(&outer));
        let mut builder = ProgramBuilder::new(crate::TargetLayout::new(64).expect("target"));
        planner
            .finish()
            .register(&mut builder)
            .unwrap_or_else(|_| panic!("nested Option plan must register"));
        assert!(builder.type_id(&outer).is_some());
    }

    fn generic_product_program() -> Program {
        let span = Span::default();
        let boxed = TypeId(0);
        let guarded = TypeId(1);
        let refined = TypeId(2);
        let text_map = TypeId(3);
        let true_contract = || Contract {
            code: "true".into(),
            span,
            expression: ContractExpr {
                kind: ContractExprKind::Constant(Constant::Bool(true)),
                span,
            },
        };
        Program {
            types: vec![
                TypeDef {
                    id: boxed,
                    name: "Boxed".into(),
                    span,
                    type_parameters: 1,
                    kind: TypeDefKind::Record {
                        fields: vec![FieldDef {
                            name: "value".into(),
                            ty: Type::Parameter(0),
                            span,
                        }],
                        invariant: None,
                    },
                },
                TypeDef {
                    id: guarded,
                    name: "Guarded".into(),
                    span,
                    type_parameters: 1,
                    kind: TypeDefKind::Record {
                        fields: vec![
                            FieldDef {
                                name: "value".into(),
                                ty: Type::Parameter(0),
                                span,
                            },
                            FieldDef {
                                name: "marker".into(),
                                ty: Type::Int,
                                span,
                            },
                        ],
                        invariant: Some(true_contract()),
                    },
                },
                TypeDef {
                    id: refined,
                    name: "RefinedBox".into(),
                    span,
                    type_parameters: 0,
                    kind: TypeDefKind::Refined {
                        base: Type::Nominal(boxed, vec![Type::Int]),
                        predicate: true_contract(),
                    },
                },
                TypeDef {
                    id: text_map,
                    name: "TextMap".into(),
                    span,
                    type_parameters: 1,
                    kind: TypeDefKind::Record {
                        fields: vec![FieldDef {
                            name: "raw".into(),
                            ty: Type::Int,
                            span,
                        }],
                        invariant: None,
                    },
                },
            ],
            prelude: PreludeIds {
                text_map: Some(text_map),
                ..PreludeIds::default()
            },
            ..Program::default()
        }
    }

    #[test]
    fn concrete_generic_products_and_refinements_register_but_text_map_stays_opaque() {
        let program = generic_product_program();
        let boxed = TypeId(0);
        let guarded = TypeId(1);
        let refined = TypeId(2);
        let text_map = TypeId(3);
        let boxed_int = Type::Nominal(boxed, vec![Type::Int]);
        let guarded_text = Type::Nominal(guarded, vec![Type::Text]);
        let refined_int = Type::Nominal(refined, Vec::new());
        let map_int = Type::Nominal(text_map, vec![Type::Int]);

        assert_eq!(
            concrete_record_fields(&program, &boxed_int).as_deref(),
            Some([Type::Int].as_slice())
        );
        assert_eq!(
            concrete_refined_base(&program, &refined_int),
            Some(boxed_int.clone())
        );
        assert!(concrete_record_fields(&program, &map_int).is_none());

        let mut planner = AggregatePlanner::new(&program, true);
        assert!(planner.supports_value_type(&boxed_int));
        assert!(planner.supports_value_type(&guarded_text));
        assert!(planner.supports_value_type(&refined_int));
        assert!(!planner.supports_value_type(&map_int));
        let mut builder = ProgramBuilder::new(crate::TargetLayout::new(64).expect("target"));
        builder
            .add_managed_text_type()
            .expect("register managed Text before products");
        planner
            .finish()
            .register(&mut builder)
            .unwrap_or_else(|_| panic!("concrete generic products must register"));
        assert!(builder.type_id(&boxed_int).is_some());
        assert!(builder.type_id(&guarded_text).is_some());
        assert!(builder.type_id(&refined_int).is_some());
        assert!(builder.type_id(&map_int).is_none());
    }
}
