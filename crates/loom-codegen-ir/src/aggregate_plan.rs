use std::collections::{BTreeMap, BTreeSet};

use loom_mir::{self as mir, Type};

use crate::{BuildError, ProgramBuilder, ValueTypeId};

pub(crate) const fn is_direct_scalar(ty: &Type) -> bool {
    matches!(ty, Type::Unit | Type::Bool | Type::Int | Type::Float)
}

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
    if usize::try_from(definition.type_parameters).ok()? != arguments.len()
        || arguments.iter().any(|argument| !is_concrete(argument))
    {
        return None;
    }
    let mir::TypeDefKind::Enum { variants } = &definition.kind else {
        return None;
    };
    if variants.is_empty() {
        return None;
    }
    variants
        .iter()
        .enumerate()
        .map(|(index, variant)| {
            if variant.id.0 as usize != index {
                return None;
            }
            variant
                .payload
                .iter()
                .map(|payload| substitute_type(payload, arguments))
                .collect::<Option<Vec<_>>>()
                .map(Vec::into_boxed_slice)
        })
        .collect::<Option<Vec<_>>>()
        .map(Vec::into_boxed_slice)
}

fn is_concrete(root: &Type) -> bool {
    let mut pending = vec![root];
    while let Some(ty) = pending.pop() {
        match ty {
            Type::Tuple(elements) => pending.extend(elements),
            Type::List(element) | Type::Task(element) | Type::TaskOutcome(element) => {
                pending.push(element);
            }
            Type::Nominal(_, arguments) => pending.extend(arguments),
            Type::View { bindings, .. } => pending.extend(bindings.values()),
            Type::Parameter(_) | Type::AssociatedProjection { .. } | Type::Error => return false,
            Type::Never | Type::Unit | Type::Bool | Type::Int | Type::Float | Type::Text => {}
        }
    }
    true
}

fn substitute_type(ty: &Type, arguments: &[Type]) -> Option<Type> {
    Some(match ty {
        Type::Parameter(index) => arguments.get(*index as usize)?.clone(),
        Type::Tuple(elements) => Type::Tuple(
            elements
                .iter()
                .map(|element| substitute_type(element, arguments))
                .collect::<Option<Vec<_>>>()?,
        ),
        Type::List(element) => Type::List(Box::new(substitute_type(element, arguments)?)),
        Type::Nominal(id, nested) => Type::Nominal(
            *id,
            nested
                .iter()
                .map(|nested| substitute_type(nested, arguments))
                .collect::<Option<Vec<_>>>()?,
        ),
        Type::Task(output) => Type::Task(Box::new(substitute_type(output, arguments)?)),
        Type::TaskOutcome(output) => {
            Type::TaskOutcome(Box::new(substitute_type(output, arguments)?))
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
                .map(|(name, ty)| Some((name.clone(), substitute_type(ty, arguments)?)))
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

/// Classifies concrete immutable products and closed sums without constructing
/// LCIR. Every root is checked against the same expanded depth/node budgets as
/// independent validation, including mixed product/sum cycles.
pub(crate) struct AggregatePlanner<'program> {
    program: &'program mir::Program,
    planned: BTreeMap<Type, AggregateShape>,
    rejected_roots: BTreeSet<Type>,
}

impl<'program> AggregatePlanner<'program> {
    pub(crate) fn new(program: &'program mir::Program) -> Self {
        Self {
            program,
            planned: BTreeMap::new(),
            rejected_roots: BTreeSet::new(),
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
            if children
                .iter()
                .any(|child| direct_aggregate_shape(self.program, child).is_none())
            {
                supported = false;
                break;
            }
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
