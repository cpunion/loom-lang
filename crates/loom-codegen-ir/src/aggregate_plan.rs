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

fn direct_aggregate_fields(program: &mir::Program, ty: &Type) -> Option<Vec<Type>> {
    match ty {
        Type::Tuple(elements) => Some(elements.clone()),
        Type::Nominal(_, _) => closed_record_fields(program, ty).map(|fields| {
            fields
                .iter()
                .map(|field| field.ty.clone())
                .collect::<Vec<_>>()
        }),
        _ => None,
    }
}

fn is_direct_aggregate(program: &mir::Program, ty: &Type) -> bool {
    matches!(ty, Type::Tuple(_)) || closed_record_fields(program, ty).is_some()
}

/// Classifies concrete immutable aggregates without constructing LCIR.
///
/// Each queried root is checked against the same expanded depth and node
/// budgets that independent LCIR validation applies. The resulting plan owns
/// every concrete tuple and record required by supported reachable values.
pub(crate) struct AggregatePlanner<'program> {
    program: &'program mir::Program,
    planned: BTreeMap<Type, Box<[Type]>>,
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

        // A frame carries an exit marker so cycle detection is path-local.
        // Repeated children are deliberately expanded again: validator budget
        // accounting counts occurrences, not only distinct type identities.
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
            let Some(fields) = direct_aggregate_fields(self.program, &semantic) else {
                supported = false;
                break;
            };
            let Some(next_structural_nodes) = structural_nodes
                .checked_add(1)
                .and_then(|nodes| nodes.checked_add(fields.len()))
            else {
                supported = false;
                break;
            };
            if next_structural_nodes > crate::repr::DIRECT_PRODUCT_MAX_STRUCTURAL_NODES {
                supported = false;
                break;
            }
            structural_nodes = next_structural_nodes;
            discovered
                .entry(semantic.clone())
                .or_insert_with(|| fields.clone().into_boxed_slice());

            let mut children = Vec::new();
            for field in &fields {
                if is_direct_scalar(field) {
                    continue;
                }
                if !is_direct_aggregate(self.program, field) {
                    supported = false;
                    break;
                }
                children.push(field.clone());
            }
            if !supported {
                break;
            }
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
    entries: BTreeMap<Type, Box<[Type]>>,
}

impl AggregatePlan {
    pub(crate) fn register(
        self,
        builder: &mut ProgramBuilder,
    ) -> Result<(), AggregateRegistrationError> {
        let mut remaining_dependencies = BTreeMap::new();
        let mut dependents: BTreeMap<Type, Vec<Type>> = BTreeMap::new();
        let mut ready = BTreeSet::new();

        for (semantic, fields) in &self.entries {
            if let Type::Tuple(elements) = semantic
                && elements.as_slice() != fields.as_ref()
            {
                return Err(AggregateRegistrationError::Inconsistent(format!(
                    "tuple plan {semantic:?} does not match its element types"
                )));
            }
            if fields
                .iter()
                .any(|field| !is_direct_scalar(field) && !self.entries.contains_key(field))
            {
                return Err(AggregateRegistrationError::Inconsistent(format!(
                    "direct aggregate {semantic:?} depends on an unplanned field type"
                )));
            }
            let dependencies = fields
                .iter()
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
            let fields = self.entries.get(&semantic).ok_or_else(|| {
                AggregateRegistrationError::Inconsistent(format!(
                    "ready aggregate {semantic:?} disappeared from its plan"
                ))
            })?;
            let _: ValueTypeId = match &semantic {
                Type::Tuple(elements) => builder.add_tuple_type(elements),
                Type::Nominal(_, arguments) if arguments.is_empty() => {
                    builder.add_pod_record_type(semantic.clone(), fields)
                }
                _ => {
                    return Err(AggregateRegistrationError::Inconsistent(format!(
                        "aggregate plan contains invalid semantic type {semantic:?}"
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
