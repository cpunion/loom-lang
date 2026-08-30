use std::collections::{BTreeMap, BTreeSet};

use loom_mir::{self as mir, ExprKind, Type};

use crate::instance_closure::InstanceSubstitution;
use crate::{InstanceKey, InstanceWitnessArgument, ReachableSourceGraph};

/// One closed-world dynamic interface proven to have exactly one reachable
/// concrete conformance in this artifact.
///
/// The proof is compiler-private. LCIR receives the concrete value directly
/// and calls the selected method instance directly, so neither a runtime type
/// tag nor a witness pointer survives into generated code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DevirtualizedView {
    proof: InstanceWitnessArgument,
    concrete: Type,
}

/// One member of an artifact-closed competing dynamic witness set. Candidate
/// order is deterministic closed-target/proof order and becomes the private
/// tag order recorded in checked LCIR. The proof itself is consumed while
/// forming each direct method [`InstanceKey`], whose canonical dump preserves
/// the specialized target identity.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct DynamicCandidate {
    concrete: Type,
    proof: InstanceWitnessArgument,
}

impl DynamicCandidate {
    pub(crate) const fn proof(&self) -> &InstanceWitnessArgument {
        &self.proof
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

    pub(crate) fn candidate(
        &self,
        concrete: &Type,
        proof: &InstanceWitnessArgument,
    ) -> Option<(u32, &DynamicCandidate)> {
        self.candidates
            .iter()
            .enumerate()
            .find(|(_, candidate)| candidate.concrete == *concrete && candidate.proof == *proof)
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
    pub(crate) const fn proof(&self) -> &InstanceWitnessArgument {
        &self.proof
    }

    pub(crate) const fn concrete(&self) -> &Type {
        &self.concrete
    }
}

/// Artifact-wide checked representation choices for the first `dyn` LCIR
/// slice.
///
/// A view is admitted only when its reachable concrete producers carry exact,
/// closed proofs whose associated bindings match the view. One candidate is
/// erased completely; two or more candidates form a checked finite dynamic
/// catalog. A producer that still needs an unavailable type or witness
/// parameter remains absent and selects structured unsupported classification
/// before LCIR construction.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct DynConceptPlan {
    choices: BTreeMap<Type, DynConceptChoice>,
}

impl DynConceptPlan {
    pub(crate) fn from_reachable(program: &mir::Program, graph: &ReachableSourceGraph) -> Self {
        let mut candidates = BTreeMap::<Type, Vec<DynamicCandidate>>::new();
        let mut invalid_views = BTreeSet::new();
        for function in graph
            .functions
            .iter()
            .filter_map(|function| program.function(*function))
        {
            let Some(producers) = graph.dynamic_producers.get(&function.id) else {
                continue;
            };
            let key = InstanceKey::monomorphic(function.id);
            let substitution = InstanceSubstitution::new(program, &key);
            for expression in function
                .exprs_preorder()
                .filter(|expression| producers.contains(&expression.id))
            {
                let ExprKind::MakeView { value, witness, .. } = &expression.kind else {
                    continue;
                };
                let Ok(view) = substitution.instantiate_type(&expression.ty) else {
                    continue;
                };
                let Ok(concrete) = substitution.instantiate_type(&value.ty) else {
                    continue;
                };
                let Ok(proof) = substitution.instantiate_witness(witness) else {
                    continue;
                };
                if !matches!(view, Type::View { .. })
                    || substitution
                        .validate_dynamic_proof(&view, &concrete, &proof)
                        .is_err()
                {
                    invalid_views.insert(view);
                    continue;
                }
                let row = candidates.entry(view.clone()).or_default();
                if let Some(existing) = row.iter().find(|candidate| candidate.concrete == concrete)
                {
                    if existing.proof != proof {
                        invalid_views.insert(view);
                    }
                    continue;
                }
                row.push(DynamicCandidate { concrete, proof });
            }
        }

        for invalid in invalid_views {
            candidates.remove(&invalid);
        }
        let choices = candidates
            .into_iter()
            .map(|(view, mut candidates)| {
                candidates.sort();
                let choice = if let [selected] = candidates.as_slice() {
                    DynConceptChoice::Unique(DevirtualizedView {
                        proof: selected.proof.clone(),
                        concrete: selected.concrete.clone(),
                    })
                } else {
                    DynConceptChoice::Finite(FiniteDynamicView {
                        candidates: candidates.into_boxed_slice(),
                    })
                };
                (view, choice)
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

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use loom_core::Span;
    use loom_mir::{
        Block, CallPlan, ConceptId, Constant, Expr, ExprId, ExprKind, FieldDef, Function,
        FunctionId, LocalDecl, LocalId, Place, Program, Statement, StatementKind, TypeDef,
        TypeDefKind, TypeId, Witness, WitnessId, WitnessRef,
    };

    use super::*;
    use crate::aggregate_plan::AggregatePlanner;
    use crate::{ProgramBuilder, TargetLayout};

    fn root_function(parameter: Type, concrete: Type, view: Type) -> Function {
        let span = Span::default();
        Function {
            id: FunctionId(0),
            name: "schema_root".into(),
            span,
            type_parameters: 0,
            is_async: false,
            suspension_points: Vec::new(),
            params: vec![
                LocalDecl {
                    id: LocalId(0),
                    name: "value".into(),
                    ty: parameter,
                    mutable: false,
                    span,
                },
                LocalDecl {
                    id: LocalId(1),
                    name: "producer".into(),
                    ty: concrete.clone(),
                    mutable: false,
                    span,
                },
            ],
            witness_params: Vec::new(),
            witness_prefix_count: 0,
            locals: Vec::new(),
            return_ty: Type::Unit,
            receiver: None,
            body: Block {
                statements: vec![Statement {
                    kind: StatementKind::Evaluate(Expr::new(
                        ExprKind::MakeView {
                            value: Box::new(Expr::new(
                                ExprKind::Copy(Place::local(LocalId(1))),
                                concrete,
                                span,
                            )),
                            writeback: None,
                            witness: WitnessRef::Concrete(WitnessId(0)),
                            mutable: false,
                            token: 0,
                        },
                        view,
                        span,
                    )),
                    span,
                }],
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
            functions: vec![root_function(
                container.clone(),
                concrete.clone(),
                view.clone(),
            )],
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
            dynamic_producers: BTreeMap::from([(
                FunctionId(0),
                BTreeSet::from([ExprId::UNASSIGNED]),
            )]),
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
            dynamic_producers: BTreeMap::from([(
                FunctionId(0),
                BTreeSet::from([ExprId::UNASSIGNED]),
            )]),
            ..ReachableSourceGraph::default()
        };
        let plan = DynConceptPlan::from_reachable(&program, &graph);
        let mut aggregates = AggregatePlanner::new(&program, &plan, true);
        assert!(!aggregates.supports_value_type(&container));
    }
}
