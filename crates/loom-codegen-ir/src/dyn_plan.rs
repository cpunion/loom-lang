use std::collections::{BTreeMap, BTreeSet};

use loom_mir::{self as mir, Type, WitnessId};

use crate::ReachableSourceGraph;

/// One closed-world dynamic interface proven to have exactly one reachable
/// concrete conformance in this artifact.
///
/// The proof is compiler-private. LCIR receives the concrete value directly
/// and calls the selected method instance directly, so neither a runtime type
/// tag nor a witness pointer survives into generated code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DevirtualizedView {
    witness: WitnessId,
    concrete: Type,
}

impl DevirtualizedView {
    pub(crate) const fn witness(&self) -> WitnessId {
        self.witness
    }

    pub(crate) const fn concrete(&self) -> &Type {
        &self.concrete
    }
}

/// Artifact-wide checked representation choices for the first `dyn` LCIR
/// slice.
///
/// A view is admitted only when the reachable witness set contains one exact,
/// non-generic conformance whose associated bindings match the view. Missing,
/// open, or competing proofs remain absent and therefore select structured
/// unsupported classification before LCIR construction.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct DynConceptPlan {
    choices: BTreeMap<Type, DevirtualizedView>,
}

impl DynConceptPlan {
    pub(crate) fn from_reachable(program: &mir::Program, graph: &ReachableSourceGraph) -> Self {
        let mut views = BTreeSet::new();
        for function in graph
            .functions
            .iter()
            .filter_map(|function| program.function(*function))
        {
            for parameter in &function.params {
                collect_views(&parameter.ty, &mut views);
            }
            collect_views(&function.return_ty, &mut views);
            for expression in function.exprs_preorder() {
                collect_views(&expression.ty, &mut views);
            }
        }

        let choices = views
            .into_iter()
            .filter_map(|view| {
                let Type::View {
                    concept, bindings, ..
                } = &view
                else {
                    return None;
                };
                let mut matches = graph
                    .witnesses
                    .iter()
                    .filter_map(|witness| program.witness(*witness))
                    .filter(|witness| {
                        witness.concept == *concept && witness.associated == *bindings
                    });
                let selected = matches.next()?;
                if matches.next().is_some()
                    || selected.type_parameters != 0
                    || !selected.prerequisites.is_empty()
                    || !is_closed_type(&selected.concrete)
                {
                    return None;
                }
                Some((
                    view,
                    DevirtualizedView {
                        witness: selected.id,
                        concrete: selected.concrete.clone(),
                    },
                ))
            })
            .collect();
        Self { choices }
    }

    pub(crate) fn choice(&self, view: &Type) -> Option<&DevirtualizedView> {
        self.choices.get(view)
    }

    /// Returns the exact LCIR semantic type after closed-world erasure. A
    /// non-view is unchanged; an unproved view has no representation.
    pub(crate) fn physical_type<'ty>(&'ty self, ty: &'ty Type) -> Option<&'ty Type> {
        match ty {
            Type::View { .. } => self.choice(ty).map(DevirtualizedView::concrete),
            _ => Some(ty),
        }
    }
}

fn collect_views(root: &Type, output: &mut BTreeSet<Type>) {
    let mut pending = vec![root];
    let mut visited = 0_usize;
    while let Some(ty) = pending.pop() {
        visited = visited.saturating_add(1);
        if visited > crate::repr::DIRECT_PRODUCT_MAX_STRUCTURAL_NODES {
            return;
        }
        match ty {
            Type::View { bindings, .. } => {
                output.insert(ty.clone());
                pending.extend(bindings.values());
            }
            Type::Tuple(elements) | Type::Nominal(_, elements) => pending.extend(elements),
            Type::List(element) | Type::Task(element) | Type::TaskOutcome(element) => {
                pending.push(element);
            }
            Type::Never
            | Type::Unit
            | Type::Bool
            | Type::Int
            | Type::Float
            | Type::Text
            | Type::Parameter(_)
            | Type::AssociatedProjection { .. }
            | Type::Error => {}
        }
    }
}

fn is_closed_type(root: &Type) -> bool {
    let mut pending = vec![root];
    let mut visited = 0_usize;
    while let Some(ty) = pending.pop() {
        visited = visited.saturating_add(1);
        if visited > crate::repr::DIRECT_PRODUCT_MAX_STRUCTURAL_NODES {
            return false;
        }
        match ty {
            Type::Tuple(elements) | Type::Nominal(_, elements) => pending.extend(elements),
            Type::List(element) | Type::Task(element) | Type::TaskOutcome(element) => {
                pending.push(element);
            }
            Type::View { bindings, .. } => pending.extend(bindings.values()),
            Type::Parameter(_) | Type::AssociatedProjection { .. } | Type::Error => return false,
            Type::Never | Type::Unit | Type::Bool | Type::Int | Type::Float | Type::Text => {}
        }
    }
    true
}
