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

fn direct_type_plan(program: &mir::Program, ty: &Type) -> Option<DirectTypePlan> {
    match ty {
        Type::Tuple(elements) => Some(DirectTypePlan::Product {
            fields: elements.clone().into_boxed_slice(),
            invariant: false,
        }),
        Type::Nominal(id, arguments) if arguments.is_empty() => {
            let definition = program.type_def(*id)?;
            if definition.type_parameters != 0 {
                return None;
            }
            match &definition.kind {
                mir::TypeDefKind::Record { fields, invariant } => Some(DirectTypePlan::Product {
                    fields: fields
                        .iter()
                        .map(|field| field.ty.clone())
                        .collect::<Vec<_>>()
                        .into_boxed_slice(),
                    invariant: invariant.is_some(),
                }),
                mir::TypeDefKind::Refined { base, .. } => {
                    Some(DirectTypePlan::Transparent { base: base.clone() })
                }
                mir::TypeDefKind::Enum { .. } => None,
            }
        }
        _ => None,
    }
}

#[derive(Clone)]
enum DirectTypePlan {
    Product {
        fields: Box<[Type]>,
        invariant: bool,
    },
    Transparent {
        base: Type,
    },
}

impl DirectTypePlan {
    fn dependencies(&self) -> Vec<Type> {
        match self {
            Self::Product { fields, .. } => fields.to_vec(),
            Self::Transparent { base } => vec![base.clone()],
        }
    }
}

/// Classifies concrete immutable aggregates without constructing LCIR.
///
/// Each queried root is checked against the same expanded depth and node
/// budgets that independent LCIR validation applies. The resulting plan owns
/// every concrete tuple and record required by supported reachable values.
pub(crate) struct AggregatePlanner<'program> {
    program: &'program mir::Program,
    planned: BTreeMap<Type, DirectTypePlan>,
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
            let Some(plan) = direct_type_plan(self.program, &semantic) else {
                supported = false;
                break;
            };
            let dependencies = plan.dependencies();
            let Some(next_structural_nodes) = structural_nodes
                .checked_add(1)
                .and_then(|nodes| nodes.checked_add(dependencies.len()))
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
                .or_insert_with(|| plan.clone());

            let mut children = Vec::new();
            for dependency in dependencies {
                if is_direct_scalar(&dependency) {
                    continue;
                }
                if direct_type_plan(self.program, &dependency).is_none() {
                    supported = false;
                    break;
                }
                children.push(dependency);
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
    entries: BTreeMap<Type, DirectTypePlan>,
}

impl AggregatePlan {
    pub(crate) fn register(
        self,
        builder: &mut ProgramBuilder,
    ) -> Result<(), AggregateRegistrationError> {
        let mut remaining_dependencies = BTreeMap::new();
        let mut dependents: BTreeMap<Type, Vec<Type>> = BTreeMap::new();
        let mut ready = BTreeSet::new();

        for (semantic, plan) in &self.entries {
            if let (Type::Tuple(elements), DirectTypePlan::Product { fields, invariant }) =
                (semantic, plan)
                && (elements.as_slice() != fields.as_ref() || *invariant)
            {
                return Err(AggregateRegistrationError::Inconsistent(format!(
                    "tuple plan {semantic:?} does not match its element types"
                )));
            }
            let plan_dependencies = plan.dependencies();
            if plan_dependencies.iter().any(|dependency| {
                !is_direct_scalar(dependency) && !self.entries.contains_key(dependency)
            }) {
                return Err(AggregateRegistrationError::Inconsistent(format!(
                    "direct type {semantic:?} depends on an unplanned value type"
                )));
            }
            let dependencies = plan_dependencies
                .into_iter()
                .filter(|dependency| self.entries.contains_key(dependency))
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
            let plan = self.entries.get(&semantic).ok_or_else(|| {
                AggregateRegistrationError::Inconsistent(format!(
                    "ready direct type {semantic:?} disappeared from its plan"
                ))
            })?;
            let _: ValueTypeId = match (&semantic, plan) {
                (
                    Type::Tuple(elements),
                    DirectTypePlan::Product {
                        invariant: false, ..
                    },
                ) => builder.add_tuple_type(elements),
                (Type::Nominal(_, arguments), DirectTypePlan::Product { fields, invariant })
                    if arguments.is_empty() =>
                {
                    if *invariant {
                        builder.add_invariant_record_type(semantic.clone(), fields)
                    } else {
                        builder.add_pod_record_type(semantic.clone(), fields)
                    }
                }
                (Type::Nominal(_, arguments), DirectTypePlan::Transparent { base })
                    if arguments.is_empty() =>
                {
                    builder.add_transparent_type(semantic.clone(), base)
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
