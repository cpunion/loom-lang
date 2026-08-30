use std::collections::{BTreeMap, BTreeSet};

use loom_mir::{self as mir, Type, WitnessId};

use crate::ReachableSourceGraph;
use crate::aggregate_plan::{
    closed_enum_variants, concrete_any_record_fields, concrete_refined_base,
};

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

/// One member of an artifact-closed competing dynamic witness set. Candidate
/// order is deterministic witness-id order and becomes the private tag order
/// recorded in checked LCIR.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DynamicCandidate {
    witness: WitnessId,
    concrete: Type,
}

impl DynamicCandidate {
    pub(crate) const fn witness(&self) -> WitnessId {
        self.witness
    }

    pub(crate) const fn concrete(&self) -> &Type {
        &self.concrete
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FiniteDynamicView {
    candidates: Box<[DynamicCandidate]>,
}

impl FiniteDynamicView {
    pub(crate) const fn candidates(&self) -> &[DynamicCandidate] {
        &self.candidates
    }

    pub(crate) fn candidate(&self, witness: WitnessId) -> Option<(u32, &DynamicCandidate)> {
        self.candidates
            .iter()
            .enumerate()
            .find(|(_, candidate)| candidate.witness == witness)
            .and_then(|(index, candidate)| {
                u32::try_from(index).ok().map(|index| (index, candidate))
            })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum DynConceptChoice {
    Unique(DevirtualizedView),
    Finite(FiniteDynamicView),
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
/// A view is admitted only when the artifact-closed reachable witness set
/// contains exact, non-generic conformances whose associated bindings match
/// the view. One candidate is erased completely; two or more candidates form
/// a checked finite dynamic catalog. Missing, open, generic, or
/// prerequisite-dependent proof sets remain absent and select structured
/// unsupported classification before LCIR construction.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct DynConceptPlan {
    choices: BTreeMap<Type, DynConceptChoice>,
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
                collect_views(program, &parameter.ty, &mut views);
            }
            collect_views(program, &function.return_ty, &mut views);
            for expression in function.exprs_preorder() {
                collect_views(program, &expression.ty, &mut views);
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
                let matches = graph
                    .witnesses
                    .iter()
                    .filter_map(|witness| program.witness(*witness))
                    .filter(|witness| {
                        witness.concept == *concept && witness.associated == *bindings
                    })
                    .collect::<Vec<_>>();
                if matches.is_empty()
                    || matches.iter().any(|selected| {
                        selected.type_parameters != 0
                            || !selected.prerequisites.is_empty()
                            || !is_closed_type(&selected.concrete)
                    })
                {
                    return None;
                }
                let candidates = matches
                    .into_iter()
                    .map(|selected| DynamicCandidate {
                        witness: selected.id,
                        concrete: selected.concrete.clone(),
                    })
                    .collect::<Vec<_>>();
                let choice = if let [selected] = candidates.as_slice() {
                    DynConceptChoice::Unique(DevirtualizedView {
                        witness: selected.witness,
                        concrete: selected.concrete.clone(),
                    })
                } else {
                    DynConceptChoice::Finite(FiniteDynamicView {
                        candidates: candidates.into_boxed_slice(),
                    })
                };
                Some((view, choice))
            })
            .collect();
        Self { choices }
    }

    pub(crate) fn choice(&self, view: &Type) -> Option<&DevirtualizedView> {
        match self.choices.get(view)? {
            DynConceptChoice::Unique(choice) => Some(choice),
            DynConceptChoice::Finite(_) => None,
        }
    }

    pub(crate) fn finite(&self, view: &Type) -> Option<&FiniteDynamicView> {
        match self.choices.get(view)? {
            DynConceptChoice::Unique(_) => None,
            DynConceptChoice::Finite(choice) => Some(choice),
        }
    }

    /// Returns the exact LCIR semantic type after closed-world erasure,
    /// recursively replacing views in aggregate and managed-container shapes.
    /// An unproved view anywhere in the finite type tree has no representation.
    pub(crate) fn physical_type(&self, ty: &Type) -> Option<Type> {
        let mut remaining = crate::repr::DIRECT_PRODUCT_MAX_STRUCTURAL_NODES;
        self.physical_type_bounded(ty, &mut remaining)
    }

    fn physical_type_bounded(&self, ty: &Type, remaining: &mut usize) -> Option<Type> {
        *remaining = remaining.checked_sub(1)?;
        Some(match ty {
            Type::View { .. } => {
                if let Some(concrete) = self.choice(ty).map(DevirtualizedView::concrete) {
                    self.physical_type_bounded(concrete, remaining)?
                } else if self.finite(ty).is_some() {
                    ty.clone()
                } else {
                    return None;
                }
            }
            Type::Tuple(elements) => Type::Tuple(
                elements
                    .iter()
                    .map(|element| self.physical_type_bounded(element, remaining))
                    .collect::<Option<Vec<_>>>()?,
            ),
            Type::List(element) => {
                Type::List(Box::new(self.physical_type_bounded(element, remaining)?))
            }
            Type::Nominal(id, arguments) => Type::Nominal(
                *id,
                arguments
                    .iter()
                    .map(|argument| self.physical_type_bounded(argument, remaining))
                    .collect::<Option<Vec<_>>>()?,
            ),
            Type::Task(output) => {
                Type::Task(Box::new(self.physical_type_bounded(output, remaining)?))
            }
            Type::TaskOutcome(output) => {
                Type::TaskOutcome(Box::new(self.physical_type_bounded(output, remaining)?))
            }
            Type::Never => Type::Never,
            Type::Unit => Type::Unit,
            Type::Bool => Type::Bool,
            Type::Int => Type::Int,
            Type::Float => Type::Float,
            Type::Text => Type::Text,
            Type::Parameter(index) => Type::Parameter(*index),
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
}

fn collect_views(program: &mir::Program, root: &Type, output: &mut BTreeSet<Type>) {
    let mut pending = vec![root.clone()];
    let mut expanded = BTreeSet::new();
    let mut visited = 0_usize;
    while let Some(ty) = pending.pop() {
        visited = visited.saturating_add(1);
        if visited > crate::repr::DIRECT_PRODUCT_MAX_STRUCTURAL_NODES {
            return;
        }
        match &ty {
            Type::View { bindings, .. } => {
                output.insert(ty.clone());
                pending.extend(bindings.values().cloned());
            }
            Type::Tuple(elements) => pending.extend(elements.iter().cloned()),
            Type::Nominal(_, arguments) => {
                pending.extend(arguments.iter().cloned());
                if !expanded.insert(ty.clone()) {
                    continue;
                }
                if let Some(fields) = concrete_any_record_fields(program, &ty) {
                    pending.extend(fields.into_vec());
                } else if let Some(variants) = closed_enum_variants(program, &ty) {
                    pending.extend(variants.into_vec().into_iter().flat_map(<[Type]>::into_vec));
                } else if let Some(base) = concrete_refined_base(program, &ty) {
                    pending.push(base);
                }
            }
            Type::List(element) | Type::Task(element) | Type::TaskOutcome(element) => {
                pending.push((**element).clone());
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

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use loom_core::Span;
    use loom_mir::{
        Block, CallPlan, ConceptId, Constant, Expr, ExprKind, FieldDef, Function, FunctionId,
        LocalDecl, LocalId, Program, TypeDef, TypeDefKind, TypeId, Witness, WitnessId,
    };

    use super::*;
    use crate::aggregate_plan::AggregatePlanner;
    use crate::{ProgramBuilder, TargetLayout};

    fn root_function(parameter: Type) -> Function {
        let span = Span::default();
        Function {
            id: FunctionId(0),
            name: "schema_root".into(),
            span,
            type_parameters: 0,
            is_async: false,
            suspension_points: Vec::new(),
            params: vec![LocalDecl {
                id: LocalId(0),
                name: "value".into(),
                ty: parameter,
                mutable: false,
                span,
            }],
            witness_params: Vec::new(),
            witness_prefix_count: 0,
            locals: Vec::new(),
            return_ty: Type::Unit,
            receiver: None,
            body: Block {
                statements: Vec::new(),
                tail: Some(Box::new(Expr::new(
                    ExprKind::Constant(Constant::Unit),
                    Type::Unit,
                    span,
                ))),
                span,
            },
            call_plan: CallPlan::default(),
        }
    }

    fn unique_schema_program(container_fields: Vec<FieldDef>) -> (Program, Type, Type, Type) {
        let span = Span::default();
        let concrete = Type::Nominal(TypeId(0), Vec::new());
        let container = Type::Nominal(TypeId(1), Vec::new());
        let view = Type::View {
            mutable: false,
            concept: ConceptId(0),
            bindings: BTreeMap::new(),
        };
        let program = Program {
            types: vec![
                TypeDef {
                    id: TypeId(0),
                    name: "Concrete".into(),
                    span,
                    type_parameters: 0,
                    kind: TypeDefKind::Record {
                        fields: vec![FieldDef {
                            name: "value".into(),
                            ty: Type::Int,
                            span,
                        }],
                        invariant: None,
                    },
                },
                TypeDef {
                    id: TypeId(1),
                    name: "Container".into(),
                    span,
                    type_parameters: 0,
                    kind: TypeDefKind::Record {
                        fields: container_fields,
                        invariant: None,
                    },
                },
            ],
            functions: vec![root_function(container.clone())],
            witnesses: vec![Witness {
                id: WitnessId(0),
                concept: ConceptId(0),
                concrete: concrete.clone(),
                methods: BTreeMap::new(),
                associated: BTreeMap::new(),
                type_parameters: 0,
                prerequisites: Vec::new(),
            }],
            ..Program::default()
        };
        (program, concrete, container, view)
    }

    #[test]
    fn reachable_nominal_schema_discovers_and_physicalizes_stored_views() {
        let span = Span::default();
        let stored_view = Type::View {
            mutable: false,
            concept: ConceptId(0),
            bindings: BTreeMap::new(),
        };
        let (program, concrete, container, view) = unique_schema_program(vec![FieldDef {
            name: "item".into(),
            ty: stored_view,
            span,
        }]);
        let graph = ReachableSourceGraph {
            functions: BTreeSet::from([FunctionId(0)]),
            witnesses: BTreeSet::from([WitnessId(0)]),
            ..ReachableSourceGraph::default()
        };
        let plan = DynConceptPlan::from_reachable(&program, &graph);
        assert_eq!(plan.physical_type(&view), Some(concrete.clone()));
        assert_eq!(
            plan.physical_type(&Type::List(Box::new(view.clone()))),
            Some(Type::List(Box::new(concrete.clone())))
        );

        let mut aggregates = AggregatePlanner::new(&program, &plan, true);
        assert!(aggregates.supports_value_type(&concrete));
        assert!(aggregates.supports_value_type(&container));
        assert!(aggregates.supports_value_type(&Type::List(Box::new(view.clone()))));
        let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        assert!(
            aggregates
                .finish()
                .register(&mut builder, &BTreeSet::new())
                .is_ok()
        );
        assert!(builder.type_id(&concrete).is_some());
        assert!(builder.type_id(&container).is_some());
        assert!(
            builder
                .type_id(&Type::List(Box::new(concrete.clone())))
                .is_some()
        );
        assert!(builder.type_id(&view).is_none());
        assert!(builder.type_id(&Type::List(Box::new(view))).is_none());
    }

    #[test]
    fn physicalized_view_field_does_not_hide_a_by_value_cycle() {
        let span = Span::default();
        let view = Type::View {
            mutable: false,
            concept: ConceptId(0),
            bindings: BTreeMap::new(),
        };
        let (mut program, _, container, _) = unique_schema_program(vec![FieldDef {
            name: "next".into(),
            ty: view,
            span,
        }]);
        program.witnesses[0].concrete = container.clone();
        let graph = ReachableSourceGraph {
            functions: BTreeSet::from([FunctionId(0)]),
            witnesses: BTreeSet::from([WitnessId(0)]),
            ..ReachableSourceGraph::default()
        };
        let plan = DynConceptPlan::from_reachable(&program, &graph);
        let mut aggregates = AggregatePlanner::new(&program, &plan, true);
        assert!(!aggregates.supports_value_type(&container));
    }
}
