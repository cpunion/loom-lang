//! Deterministic concept conformance indexing and proof search.

use std::collections::BTreeMap;

use loom_hir::{DefId, GenericParamId};
use serde::{Deserialize, Serialize};

use crate::{AssociatedTypeBinding, ConceptInstance, Substitution, TyData, TyId, TyInterner};

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct Goal {
    pub self_ty: TyId,
    pub concept: ConceptInstance,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ParamEnv {
    pub bounds: Vec<Goal>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ImplHeader {
    pub definition: DefId,
    pub generic_params: Vec<GenericParamId>,
    pub concept: DefId,
    pub target: TyId,
    pub conditions: Vec<Goal>,
    pub associated_types: Vec<AssociatedTypeBinding>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ImplIndex {
    by_concept: BTreeMap<DefId, Vec<ImplHeader>>,
}

impl ImplIndex {
    pub fn insert(&mut self, header: ImplHeader) {
        self.by_concept
            .entry(header.concept)
            .or_default()
            .push(header);
    }

    pub fn finish(&mut self) {
        for headers in self.by_concept.values_mut() {
            headers.sort_by_key(|header| header.definition);
        }
    }

    #[must_use]
    pub fn for_concept(&self, concept: DefId) -> &[ImplHeader] {
        self.by_concept.get(&concept).map_or(&[], Vec::as_slice)
    }

    #[must_use]
    pub fn header(&self, definition: DefId) -> Option<&ImplHeader> {
        self.by_concept
            .values()
            .flatten()
            .find(|header| header.definition == definition)
    }

    /// Returns all pairs whose target heads might overlap. Conditions are
    /// intentionally ignored, as required by Core 0.2's conservative rule.
    #[must_use]
    pub fn overlapping_pairs(&self, types: &TyInterner) -> Vec<(DefId, DefId)> {
        let mut overlaps = Vec::new();
        for headers in self.by_concept.values() {
            for (index, left) in headers.iter().enumerate() {
                for right in &headers[index + 1..] {
                    if heads_may_unify(types, left.target, right.target) {
                        overlaps.push((left.definition, right.definition));
                    }
                }
            }
        }
        overlaps.sort_unstable();
        overlaps
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WitnessSelection {
    pub source: WitnessSource,
    pub substitution: Substitution,
    pub associated_types: Vec<AssociatedTypeBinding>,
    pub prerequisites: Vec<WitnessSelection>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum WitnessSource {
    Implementation(DefId),
    ParamBound(usize),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SolveFailure {
    Missing,
    Ambiguous(Vec<DefId>),
    Cycle(Vec<Goal>),
    AssociatedTypeMismatch {
        associated_type: DefId,
        expected: TyId,
        actual: TyId,
    },
}

pub struct ConformanceSolver<'a> {
    index: &'a ImplIndex,
    types: &'a mut TyInterner,
    stack: Vec<Goal>,
}

impl<'a> ConformanceSolver<'a> {
    pub fn new(index: &'a ImplIndex, types: &'a mut TyInterner) -> Self {
        Self {
            index,
            types,
            stack: Vec::new(),
        }
    }

    /// Selects the unique global implementation or matching parameter proof.
    ///
    /// # Errors
    ///
    /// Returns a structured failure when no proof exists, multiple proofs are
    /// applicable, proof search cycles, or associated bindings disagree.
    pub fn solve(
        &mut self,
        goal: &Goal,
        environment: &ParamEnv,
    ) -> Result<WitnessSelection, SolveFailure> {
        self.solve_inner(goal, environment)
    }

    fn solve_inner(
        &mut self,
        goal: &Goal,
        environment: &ParamEnv,
    ) -> Result<WitnessSelection, SolveFailure> {
        if let Some(start) = self.stack.iter().position(|active| active == goal) {
            let mut cycle = self.stack[start..].to_vec();
            cycle.push(goal.clone());
            return Err(SolveFailure::Cycle(cycle));
        }

        if let Some((bound_index, bound)) = environment
            .bounds
            .iter()
            .enumerate()
            .find(|(_, bound)| *bound == goal)
        {
            return Ok(WitnessSelection {
                source: WitnessSource::ParamBound(bound_index),
                substitution: Substitution::default(),
                associated_types: bound.concept.bindings.clone(),
                prerequisites: Vec::new(),
            });
        }

        self.stack.push(goal.clone());
        let result = self.select_global(goal, environment);
        self.stack.pop();
        result
    }

    fn select_global(
        &mut self,
        goal: &Goal,
        environment: &ParamEnv,
    ) -> Result<WitnessSelection, SolveFailure> {
        let mut successes = Vec::new();
        for header in self.index.for_concept(goal.concept.concept) {
            let mut substitution = Substitution::default();
            if !match_target(self.types, header.target, goal.self_ty, &mut substitution) {
                continue;
            }

            let mut prerequisites = Vec::new();
            let mut failed = false;
            for condition in &header.conditions {
                let instantiated = Goal {
                    self_ty: self.types.substitute(condition.self_ty, &substitution),
                    concept: substitute_concept(self.types, &condition.concept, &substitution),
                };
                match self.solve_inner(&instantiated, environment) {
                    Ok(selection) => prerequisites.push(selection),
                    Err(SolveFailure::Missing) => {
                        failed = true;
                        break;
                    }
                    Err(other) => return Err(other),
                }
            }
            if failed {
                continue;
            }

            let associated_types = header
                .associated_types
                .iter()
                .map(|binding| AssociatedTypeBinding {
                    associated_type: binding.associated_type,
                    ty: self.types.substitute(binding.ty, &substitution),
                })
                .collect::<Vec<_>>();
            check_associated_bindings(&goal.concept.bindings, &associated_types)?;
            successes.push(WitnessSelection {
                source: WitnessSource::Implementation(header.definition),
                substitution,
                associated_types,
                prerequisites,
            });
        }

        match successes.len() {
            0 => Err(SolveFailure::Missing),
            1 => Ok(successes.pop().expect("length checked")),
            _ => Err(SolveFailure::Ambiguous(
                successes
                    .into_iter()
                    .filter_map(|selection| match selection.source {
                        WitnessSource::Implementation(implementation) => Some(implementation),
                        WitnessSource::ParamBound(_) => None,
                    })
                    .collect(),
            )),
        }
    }
}

fn substitute_concept(
    types: &mut TyInterner,
    concept: &ConceptInstance,
    substitution: &Substitution,
) -> ConceptInstance {
    ConceptInstance {
        concept: concept.concept,
        bindings: concept
            .bindings
            .iter()
            .map(|binding| AssociatedTypeBinding {
                associated_type: binding.associated_type,
                ty: types.substitute(binding.ty, substitution),
            })
            .collect(),
    }
}

fn check_associated_bindings(
    expected: &[AssociatedTypeBinding],
    actual: &[AssociatedTypeBinding],
) -> Result<(), SolveFailure> {
    let actual = actual
        .iter()
        .map(|binding| (binding.associated_type, binding.ty))
        .collect::<BTreeMap<_, _>>();
    for binding in expected {
        let Some(actual_ty) = actual.get(&binding.associated_type) else {
            return Err(SolveFailure::Missing);
        };
        if *actual_ty != binding.ty {
            return Err(SolveFailure::AssociatedTypeMismatch {
                associated_type: binding.associated_type,
                expected: binding.ty,
                actual: *actual_ty,
            });
        }
    }
    Ok(())
}

fn match_target(
    types: &TyInterner,
    pattern: TyId,
    actual: TyId,
    substitution: &mut Substitution,
) -> bool {
    match (types.data(pattern), types.data(actual)) {
        (TyData::Param(parameter), _) => {
            if let Some(previous) = substitution.get(*parameter) {
                previous == actual
            } else {
                substitution.insert(*parameter, actual);
                true
            }
        }
        (
            TyData::Nominal {
                definition: left_definition,
                arguments: left_arguments,
            },
            TyData::Nominal {
                definition: right_definition,
                arguments: right_arguments,
            },
        ) => {
            left_definition == right_definition
                && left_arguments.len() == right_arguments.len()
                && left_arguments
                    .iter()
                    .zip(right_arguments)
                    .all(|(left, right)| match_target(types, *left, *right, substitution))
        }
        (TyData::Option(left), TyData::Option(right)) => {
            match_target(types, *left, *right, substitution)
        }
        (TyData::TextMap(left), TyData::TextMap(right)) => {
            match_target(types, *left, *right, substitution)
        }
        (
            TyData::Result {
                ok: left_ok,
                error: left_error,
            },
            TyData::Result {
                ok: right_ok,
                error: right_error,
            },
        ) => {
            match_target(types, *left_ok, *right_ok, substitution)
                && match_target(types, *left_error, *right_error, substitution)
        }
        (left, right) => left == right,
    }
}

fn heads_may_unify(types: &TyInterner, left: TyId, right: TyId) -> bool {
    let mut left_bindings = BTreeMap::new();
    let mut right_bindings = BTreeMap::new();
    heads_may_unify_inner(types, left, right, &mut left_bindings, &mut right_bindings)
}

fn heads_may_unify_inner(
    types: &TyInterner,
    left: TyId,
    right: TyId,
    left_bindings: &mut BTreeMap<GenericParamId, TyId>,
    right_bindings: &mut BTreeMap<GenericParamId, TyId>,
) -> bool {
    match (types.data(left), types.data(right)) {
        (TyData::Param(parameter), _) => bind_overlap_param(*parameter, right, left_bindings),
        (_, TyData::Param(parameter)) => bind_overlap_param(*parameter, left, right_bindings),
        (
            TyData::Nominal {
                definition: left_definition,
                arguments: left_arguments,
            },
            TyData::Nominal {
                definition: right_definition,
                arguments: right_arguments,
            },
        ) => {
            left_definition == right_definition
                && left_arguments.len() == right_arguments.len()
                && left_arguments
                    .iter()
                    .zip(right_arguments)
                    .all(|(left, right)| {
                        heads_may_unify_inner(types, *left, *right, left_bindings, right_bindings)
                    })
        }
        (TyData::Option(left), TyData::Option(right)) => {
            heads_may_unify_inner(types, *left, *right, left_bindings, right_bindings)
        }
        (TyData::TextMap(left), TyData::TextMap(right)) => {
            heads_may_unify_inner(types, *left, *right, left_bindings, right_bindings)
        }
        (
            TyData::Result {
                ok: left_ok,
                error: left_error,
            },
            TyData::Result {
                ok: right_ok,
                error: right_error,
            },
        ) => {
            heads_may_unify_inner(types, *left_ok, *right_ok, left_bindings, right_bindings)
                && heads_may_unify_inner(
                    types,
                    *left_error,
                    *right_error,
                    left_bindings,
                    right_bindings,
                )
        }
        // Projections can normalize differently under later proofs, so overlap
        // checking must conservatively assume they may unify.
        (TyData::Projection { .. }, _) | (_, TyData::Projection { .. }) => true,
        (left, right) => left == right,
    }
}

fn bind_overlap_param(
    parameter: GenericParamId,
    ty: TyId,
    bindings: &mut BTreeMap<GenericParamId, TyId>,
) -> bool {
    if let Some(previous) = bindings.get(&parameter) {
        *previous == ty
    } else {
        bindings.insert(parameter, ty);
        true
    }
}

#[cfg(test)]
mod tests {
    use loom_hir::{DefId, GenericParamId};

    use super::{ConformanceSolver, Goal, ImplHeader, ImplIndex, ParamEnv, SolveFailure};
    use crate::{BuiltinType, ConceptInstance, TyData, TyInterner};

    #[test]
    fn conditional_conformance_uses_only_available_parameter_proof() {
        let mut types = TyInterner::new();
        let equatable = DefId::from_raw(1);
        let boxed = DefId::from_raw(2);
        let text = DefId::from_raw(3);
        let parameter = GenericParamId::from_raw(0);
        let parameter_ty = types.intern(TyData::Param(parameter));
        let boxed_parameter = types.intern(TyData::Nominal {
            definition: boxed,
            arguments: vec![parameter_ty],
        });
        let text_ty = types.intern(TyData::Nominal {
            definition: text,
            arguments: Vec::new(),
        });
        let boxed_text = types.intern(TyData::Nominal {
            definition: boxed,
            arguments: vec![text_ty],
        });
        let concept = ConceptInstance {
            concept: equatable,
            bindings: Vec::new(),
        };

        let mut index = ImplIndex::default();
        index.insert(ImplHeader {
            definition: DefId::from_raw(10),
            generic_params: vec![parameter],
            concept: equatable,
            target: boxed_parameter,
            conditions: vec![Goal {
                self_ty: parameter_ty,
                concept: concept.clone(),
            }],
            associated_types: Vec::new(),
        });
        index.finish();

        let goal = Goal {
            self_ty: boxed_text,
            concept: concept.clone(),
        };
        let mut solver = ConformanceSolver::new(&index, &mut types);
        assert_eq!(
            solver.solve(&goal, &ParamEnv::default()),
            Err(SolveFailure::Missing)
        );

        let environment = ParamEnv {
            bounds: vec![Goal {
                self_ty: text_ty,
                concept,
            }],
        };
        let selected = solver.solve(&goal, &environment).unwrap();
        assert_eq!(
            selected.source,
            super::WitnessSource::Implementation(DefId::from_raw(10))
        );
    }

    #[test]
    fn overlap_is_checked_only_within_each_concept_bucket() {
        let mut types = TyInterner::new();
        let price = types.intern(TyData::Nominal {
            definition: DefId::from_raw(5),
            arguments: Vec::new(),
        });
        let mut index = ImplIndex::default();
        for (implementation, concept) in [(10, 1), (11, 2)] {
            index.insert(ImplHeader {
                definition: DefId::from_raw(implementation),
                generic_params: Vec::new(),
                concept: DefId::from_raw(concept),
                target: price,
                conditions: Vec::new(),
                associated_types: Vec::new(),
            });
        }
        assert!(index.overlapping_pairs(&types).is_empty());
    }

    #[test]
    fn text_map_heads_substitute_their_value_parameter() {
        let mut types = TyInterner::new();
        let concept = DefId::from_raw(1);
        let parameter = GenericParamId::from_raw(0);
        let parameter_ty = types.intern(TyData::Param(parameter));
        let map_parameter = types.intern(TyData::TextMap(parameter_ty));
        let text = types.builtin(BuiltinType::Text);
        let map_text = types.intern(TyData::TextMap(text));
        let implementation = DefId::from_raw(10);

        let mut index = ImplIndex::default();
        index.insert(ImplHeader {
            definition: implementation,
            generic_params: vec![parameter],
            concept,
            target: map_parameter,
            conditions: Vec::new(),
            associated_types: Vec::new(),
        });
        index.finish();

        let goal = Goal {
            self_ty: map_text,
            concept: ConceptInstance {
                concept,
                bindings: Vec::new(),
            },
        };
        let mut solver = ConformanceSolver::new(&index, &mut types);
        let selected = solver.solve(&goal, &ParamEnv::default()).unwrap();
        assert_eq!(
            selected.source,
            super::WitnessSource::Implementation(implementation)
        );
        assert_eq!(selected.substitution.get(parameter), Some(text));
    }
}
