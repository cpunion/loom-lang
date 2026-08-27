use std::collections::BTreeMap;

use loom_core::Span;
use loom_mir::{
    self as mir, CallArgument, CallTarget, ConceptId, ExprId, ExprKind, FunctionId, RequirementId,
    StatementKind, Type, WitnessRef,
};

use crate::dyn_plan::DynConceptPlan;
use crate::{INSTANCE_KEY_STRUCTURE_BUDGET, InstanceKey, InstanceWitnessArgument};

/// Maximum number of concrete callable instances in one LCIR artifact.
///
/// This is an implementation resource bound rather than a source-language
/// promise. It is deliberately checked while planning, before any LCIR table
/// or instantiated MIR type is allocated.
pub const INSTANCE_CLOSURE_MAX_INSTANCES: usize = 4_096;

/// Maximum number of reachable direct-call edges in one LCIR artifact.
pub const INSTANCE_CLOSURE_MAX_CALL_EDGES: usize = 16_384;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InstanceClosureUnsupportedKind {
    InstanceBudget,
    NonRegularRecursion,
    Instantiation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InstanceClosureUnsupported {
    pub(crate) kind: InstanceClosureUnsupportedKind,
    pub(crate) function: FunctionId,
    pub(crate) expression: Option<ExprId>,
    pub(crate) span: Span,
    pub(crate) path: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InstanceClosureError {
    MissingFunction(FunctionId),
    InvalidInstanceArity {
        function: FunctionId,
        expected_types: usize,
        actual_types: usize,
        expected_witnesses: usize,
        actual_witnesses: usize,
    },
    InvalidCheckedInstantiation {
        function: FunctionId,
        expression: ExprId,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct InstanceClosure {
    entries: Vec<InstanceKey>,
    calls: BTreeMap<String, Box<[InstanceKey]>>,
}

impl InstanceClosure {
    pub(crate) fn entries(&self) -> &[InstanceKey] {
        &self.entries
    }

    pub(crate) fn calls(&self, caller: &InstanceKey) -> Option<&[InstanceKey]> {
        self.calls
            .get(&caller.canonical_identity())
            .map(Box::as_ref)
    }
}

pub(crate) enum InstanceClosureOutcome {
    Complete(InstanceClosure),
    Unsupported(InstanceClosureUnsupported),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InstantiationError {
    StructureBudget,
    UnboundTypeParameter,
    UnboundWitnessParameter,
    UnresolvedAssociatedProjection,
    InvalidCheckedWitnessMetadata,
}

/// A bounded view of one concrete source-function instance.
///
/// Every public operation performs a node-count preflight before cloning any
/// substituted type or witness tree. This keeps polymorphic expansion an
/// atomic route-selection concern instead of a late allocator failure.
pub(crate) struct InstanceSubstitution<'program, 'key> {
    program: &'program mir::Program,
    key: &'key InstanceKey,
}

impl<'program, 'key> InstanceSubstitution<'program, 'key> {
    pub(crate) const fn new(program: &'program mir::Program, key: &'key InstanceKey) -> Self {
        Self { program, key }
    }

    pub(crate) fn instantiate_type(&self, ty: &Type) -> Result<Type, InstantiationError> {
        let mut budget = StructureBudget::default();
        self.clone_caller_type(ty, &mut budget, &mut Vec::new())
    }

    pub(crate) fn instantiate_types(
        &self,
        types: &[Type],
    ) -> Result<Vec<Type>, InstantiationError> {
        let mut budget = StructureBudget::default();
        let mut active_projections = Vec::new();
        types
            .iter()
            .map(|ty| self.clone_caller_type(ty, &mut budget, &mut active_projections))
            .collect()
    }

    pub(crate) fn call_key(
        &self,
        callee: FunctionId,
        type_arguments: &[Type],
        witnesses: &[WitnessRef],
    ) -> Result<InstanceKey, InstantiationError> {
        let mut budget = StructureBudget::default();
        let mut active_projections = Vec::new();
        let types = type_arguments
            .iter()
            .map(|ty| self.clone_caller_type(ty, &mut budget, &mut active_projections))
            .collect::<Result<Vec<_>, _>>()?;
        if types.iter().any(type_has_open_component) {
            return Err(InstantiationError::UnboundTypeParameter);
        }
        let witnesses = witnesses
            .iter()
            .map(|witness| self.clone_witness(witness, &mut budget))
            .collect::<Result<Vec<_>, _>>()?;
        Self::finish_key(callee, types, witnesses)
    }

    fn clone_witness(
        &self,
        witness: &WitnessRef,
        budget: &mut StructureBudget,
    ) -> Result<InstanceWitnessArgument, InstantiationError> {
        match witness {
            WitnessRef::Concrete(witness) => {
                budget.charge()?;
                Ok(InstanceWitnessArgument::Concrete(*witness))
            }
            WitnessRef::Parameter(index) => self
                .key
                .witness_arguments()
                .get(*index as usize)
                .ok_or(InstantiationError::UnboundWitnessParameter)
                .and_then(|witness| clone_concrete_witness(witness, budget)),
            WitnessRef::Apply { witness, arguments } => {
                budget.charge()?;
                Ok(InstanceWitnessArgument::apply(
                    *witness,
                    arguments
                        .iter()
                        .map(|argument| self.clone_witness(argument, budget))
                        .collect::<Result<Vec<_>, _>>()?,
                ))
            }
        }
    }

    /// Resolves one checked static concept call into the ordinary concrete
    /// method instance that LCIR will call directly.
    pub(crate) fn static_call_key(
        &self,
        requirement: RequirementId,
        witness: &WitnessRef,
        dispatch_type: &Type,
        method_type_arguments: &[Type],
        method_witnesses: &[WitnessRef],
    ) -> Result<InstanceKey, InstantiationError> {
        let requirement = self
            .program
            .requirement(requirement)
            .ok_or(InstantiationError::InvalidCheckedWitnessMetadata)?;
        let dispatch_type = self.instantiate_type(dispatch_type)?;
        let mut proof_budget = StructureBudget::default();
        let proof = self.clone_witness(witness, &mut proof_budget)?;
        let resolved = self.resolve_proof(&proof, &dispatch_type, requirement.concept)?;
        let callee = resolved
            .definition
            .methods
            .get(&requirement.id)
            .copied()
            .ok_or(InstantiationError::InvalidCheckedWitnessMetadata)?;

        let mut budget = StructureBudget::default();
        let mut type_arguments = resolved
            .type_arguments
            .iter()
            .map(|argument| clone_closed_type(argument, &mut budget))
            .collect::<Result<Vec<_>, _>>()?;
        let mut active_projections = Vec::new();
        type_arguments.extend(
            method_type_arguments
                .iter()
                .map(|argument| {
                    self.clone_caller_type(argument, &mut budget, &mut active_projections)
                })
                .collect::<Result<Vec<_>, _>>()?,
        );

        let mut witness_arguments = match proof {
            InstanceWitnessArgument::Concrete(_) => Vec::new(),
            InstanceWitnessArgument::Apply { arguments, .. } => arguments.into_vec(),
            InstanceWitnessArgument::Parameter(_) => {
                return Err(InstantiationError::UnboundWitnessParameter);
            }
        };
        for argument in &witness_arguments {
            budget.charge_witness_tree(argument)?;
        }
        witness_arguments.extend(
            method_witnesses
                .iter()
                .map(|witness| self.clone_witness(witness, &mut budget))
                .collect::<Result<Vec<_>, _>>()?,
        );
        Self::finish_key(callee, type_arguments, witness_arguments)
    }

    fn finish_key(
        callee: FunctionId,
        types: Vec<Type>,
        witnesses: Vec<InstanceWitnessArgument>,
    ) -> Result<InstanceKey, InstantiationError> {
        if types.iter().any(type_has_open_component) {
            return Err(InstantiationError::UnboundTypeParameter);
        }
        if witnesses.iter().any(witness_has_parameter) {
            return Err(InstantiationError::UnboundWitnessParameter);
        }
        let key = InstanceKey::new(callee, types, witnesses);
        key.validate_structure()
            .map_err(|_| InstantiationError::StructureBudget)?;
        Ok(key)
    }

    fn clone_caller_type(
        &self,
        ty: &Type,
        budget: &mut StructureBudget,
        active_projections: &mut Vec<u32>,
    ) -> Result<Type, InstantiationError> {
        match ty {
            Type::Parameter(index) => {
                let argument = self
                    .key
                    .type_arguments()
                    .get(*index as usize)
                    .ok_or(InstantiationError::UnboundTypeParameter)?;
                clone_closed_type(argument, budget)
            }
            Type::AssociatedProjection {
                witness,
                associated,
            } => self.clone_associated_projection(*witness, associated, budget, active_projections),
            Type::Tuple(elements) => {
                budget.charge()?;
                Ok(Type::Tuple(
                    elements
                        .iter()
                        .map(|element| self.clone_caller_type(element, budget, active_projections))
                        .collect::<Result<_, _>>()?,
                ))
            }
            Type::List(element) => {
                budget.charge()?;
                Ok(Type::List(Box::new(self.clone_caller_type(
                    element,
                    budget,
                    active_projections,
                )?)))
            }
            Type::Nominal(id, arguments) => {
                budget.charge()?;
                Ok(Type::Nominal(
                    *id,
                    arguments
                        .iter()
                        .map(|argument| {
                            self.clone_caller_type(argument, budget, active_projections)
                        })
                        .collect::<Result<_, _>>()?,
                ))
            }
            Type::Task(output) => {
                budget.charge()?;
                Ok(Type::Task(Box::new(self.clone_caller_type(
                    output,
                    budget,
                    active_projections,
                )?)))
            }
            Type::TaskOutcome(output) => {
                budget.charge()?;
                Ok(Type::TaskOutcome(Box::new(self.clone_caller_type(
                    output,
                    budget,
                    active_projections,
                )?)))
            }
            Type::View {
                mutable,
                concept,
                bindings,
            } => {
                budget.charge()?;
                Ok(Type::View {
                    mutable: *mutable,
                    concept: *concept,
                    bindings: bindings
                        .iter()
                        .map(|(name, ty)| {
                            Ok((
                                name.clone(),
                                self.clone_caller_type(ty, budget, active_projections)?,
                            ))
                        })
                        .collect::<Result<_, InstantiationError>>()?,
                })
            }
            Type::Never => clone_leaf(Type::Never, budget),
            Type::Unit => clone_leaf(Type::Unit, budget),
            Type::Bool => clone_leaf(Type::Bool, budget),
            Type::Int => clone_leaf(Type::Int, budget),
            Type::Float => clone_leaf(Type::Float, budget),
            Type::Text => clone_leaf(Type::Text, budget),
            Type::Error => clone_leaf(Type::Error, budget),
        }
    }

    fn clone_associated_projection(
        &self,
        witness_index: u32,
        associated: &str,
        budget: &mut StructureBudget,
        active_projections: &mut Vec<u32>,
    ) -> Result<Type, InstantiationError> {
        if active_projections.contains(&witness_index) {
            return Err(InstantiationError::UnresolvedAssociatedProjection);
        }
        let function = self
            .program
            .function(self.key.source())
            .ok_or(InstantiationError::InvalidCheckedWitnessMetadata)?;
        let parameter = function
            .witness_params
            .get(witness_index as usize)
            .ok_or(InstantiationError::UnboundWitnessParameter)?;
        let proof = self
            .key
            .witness_arguments()
            .get(witness_index as usize)
            .ok_or(InstantiationError::UnboundWitnessParameter)?;

        active_projections.push(witness_index);
        let mut expectation_budget = StructureBudget::default();
        let target = self.clone_caller_type(
            &parameter.target,
            &mut expectation_budget,
            active_projections,
        );
        let result = target.and_then(|target| {
            let resolved = self.resolve_proof(proof, &target, parameter.concept)?;
            let binding = resolved
                .definition
                .associated
                .get(associated)
                .ok_or(InstantiationError::InvalidCheckedWitnessMetadata)?;
            clone_schema_type(binding, &resolved.type_arguments, budget)
        });
        active_projections.pop();
        result
    }

    fn resolve_proof<'target>(
        &self,
        proof: &InstanceWitnessArgument,
        expected_target: &'target Type,
        expected_concept: ConceptId,
    ) -> Result<ResolvedProof<'program, 'target>, InstantiationError> {
        let mut remaining = INSTANCE_KEY_STRUCTURE_BUDGET;
        self.resolve_proof_bounded(proof, expected_target, expected_concept, &mut remaining)
    }

    fn resolve_proof_bounded<'target>(
        &self,
        proof: &InstanceWitnessArgument,
        expected_target: &'target Type,
        expected_concept: ConceptId,
        remaining: &mut usize,
    ) -> Result<ResolvedProof<'program, 'target>, InstantiationError> {
        *remaining = remaining
            .checked_sub(1)
            .ok_or(InstantiationError::StructureBudget)?;
        let (witness_id, proof_arguments, applied) = match proof {
            InstanceWitnessArgument::Concrete(witness) => (*witness, &[][..], false),
            InstanceWitnessArgument::Apply { witness, arguments } => {
                (*witness, arguments.as_ref(), true)
            }
            InstanceWitnessArgument::Parameter(_) => {
                return Err(InstantiationError::UnboundWitnessParameter);
            }
        };
        let definition = self
            .program
            .witness(witness_id)
            .ok_or(InstantiationError::InvalidCheckedWitnessMetadata)?;
        if definition.concept != expected_concept
            || applied != (definition.type_parameters != 0 || !definition.prerequisites.is_empty())
            || proof_arguments.len() != definition.prerequisites.len()
        {
            return Err(InstantiationError::InvalidCheckedWitnessMetadata);
        }
        let type_arguments = infer_head_arguments(
            &definition.concrete,
            expected_target,
            definition.type_parameters,
        )?;
        for (argument, prerequisite) in proof_arguments.iter().zip(&definition.prerequisites) {
            let mut schema_budget = StructureBudget::default();
            let target =
                clone_schema_type(&prerequisite.target, &type_arguments, &mut schema_budget)?;
            self.resolve_proof_bounded(argument, &target, prerequisite.concept, remaining)?;
        }
        Ok(ResolvedProof {
            definition,
            type_arguments,
        })
    }
}

struct ResolvedProof<'program, 'target> {
    definition: &'program mir::Witness,
    type_arguments: Vec<&'target Type>,
}

#[derive(Default)]
struct StructureBudget {
    nodes: usize,
}

impl StructureBudget {
    fn charge(&mut self) -> Result<(), InstantiationError> {
        self.nodes = self
            .nodes
            .checked_add(1)
            .ok_or(InstantiationError::StructureBudget)?;
        if self.nodes > INSTANCE_KEY_STRUCTURE_BUDGET {
            return Err(InstantiationError::StructureBudget);
        }
        Ok(())
    }

    fn charge_witness_tree(
        &mut self,
        root: &InstanceWitnessArgument,
    ) -> Result<(), InstantiationError> {
        let mut pending = vec![root];
        while let Some(witness) = pending.pop() {
            self.charge()?;
            match witness {
                InstanceWitnessArgument::Concrete(_) => {}
                InstanceWitnessArgument::Parameter(_) => {
                    return Err(InstantiationError::UnboundWitnessParameter);
                }
                InstanceWitnessArgument::Apply { arguments, .. } => {
                    pending.extend(arguments.iter());
                }
            }
        }
        Ok(())
    }
}

fn clone_leaf(ty: Type, budget: &mut StructureBudget) -> Result<Type, InstantiationError> {
    budget.charge()?;
    Ok(ty)
}

fn clone_closed_type(ty: &Type, budget: &mut StructureBudget) -> Result<Type, InstantiationError> {
    Ok(match ty {
        Type::Parameter(_) => return Err(InstantiationError::UnboundTypeParameter),
        Type::AssociatedProjection { .. } => {
            return Err(InstantiationError::UnresolvedAssociatedProjection);
        }
        Type::Tuple(elements) => {
            budget.charge()?;
            Type::Tuple(
                elements
                    .iter()
                    .map(|element| clone_closed_type(element, budget))
                    .collect::<Result<_, _>>()?,
            )
        }
        Type::List(element) => {
            budget.charge()?;
            Type::List(Box::new(clone_closed_type(element, budget)?))
        }
        Type::Nominal(id, arguments) => {
            budget.charge()?;
            Type::Nominal(
                *id,
                arguments
                    .iter()
                    .map(|argument| clone_closed_type(argument, budget))
                    .collect::<Result<_, _>>()?,
            )
        }
        Type::Task(output) => {
            budget.charge()?;
            Type::Task(Box::new(clone_closed_type(output, budget)?))
        }
        Type::TaskOutcome(output) => {
            budget.charge()?;
            Type::TaskOutcome(Box::new(clone_closed_type(output, budget)?))
        }
        Type::View {
            mutable,
            concept,
            bindings,
        } => {
            budget.charge()?;
            Type::View {
                mutable: *mutable,
                concept: *concept,
                bindings: bindings
                    .iter()
                    .map(|(name, ty)| Ok((name.clone(), clone_closed_type(ty, budget)?)))
                    .collect::<Result<_, InstantiationError>>()?,
            }
        }
        Type::Never => return clone_leaf(Type::Never, budget),
        Type::Unit => return clone_leaf(Type::Unit, budget),
        Type::Bool => return clone_leaf(Type::Bool, budget),
        Type::Int => return clone_leaf(Type::Int, budget),
        Type::Float => return clone_leaf(Type::Float, budget),
        Type::Text => return clone_leaf(Type::Text, budget),
        Type::Error => return clone_leaf(Type::Error, budget),
    })
}

fn clone_schema_type(
    ty: &Type,
    type_arguments: &[&Type],
    budget: &mut StructureBudget,
) -> Result<Type, InstantiationError> {
    match ty {
        Type::Parameter(index) => type_arguments
            .get(*index as usize)
            .ok_or(InstantiationError::InvalidCheckedWitnessMetadata)
            .and_then(|argument| clone_closed_type(argument, budget)),
        Type::AssociatedProjection { .. } => Err(InstantiationError::InvalidCheckedWitnessMetadata),
        Type::Tuple(elements) => {
            budget.charge()?;
            Ok(Type::Tuple(
                elements
                    .iter()
                    .map(|element| clone_schema_type(element, type_arguments, budget))
                    .collect::<Result<_, _>>()?,
            ))
        }
        Type::List(element) => {
            budget.charge()?;
            Ok(Type::List(Box::new(clone_schema_type(
                element,
                type_arguments,
                budget,
            )?)))
        }
        Type::Nominal(id, arguments) => {
            budget.charge()?;
            Ok(Type::Nominal(
                *id,
                arguments
                    .iter()
                    .map(|argument| clone_schema_type(argument, type_arguments, budget))
                    .collect::<Result<_, _>>()?,
            ))
        }
        Type::Task(output) => {
            budget.charge()?;
            Ok(Type::Task(Box::new(clone_schema_type(
                output,
                type_arguments,
                budget,
            )?)))
        }
        Type::TaskOutcome(output) => {
            budget.charge()?;
            Ok(Type::TaskOutcome(Box::new(clone_schema_type(
                output,
                type_arguments,
                budget,
            )?)))
        }
        Type::View {
            mutable,
            concept,
            bindings,
        } => {
            budget.charge()?;
            Ok(Type::View {
                mutable: *mutable,
                concept: *concept,
                bindings: bindings
                    .iter()
                    .map(|(name, ty)| {
                        Ok((name.clone(), clone_schema_type(ty, type_arguments, budget)?))
                    })
                    .collect::<Result<_, InstantiationError>>()?,
            })
        }
        Type::Never => clone_leaf(Type::Never, budget),
        Type::Unit => clone_leaf(Type::Unit, budget),
        Type::Bool => clone_leaf(Type::Bool, budget),
        Type::Int => clone_leaf(Type::Int, budget),
        Type::Float => clone_leaf(Type::Float, budget),
        Type::Text => clone_leaf(Type::Text, budget),
        Type::Error => clone_leaf(Type::Error, budget),
    }
}

fn clone_concrete_witness(
    witness: &InstanceWitnessArgument,
    budget: &mut StructureBudget,
) -> Result<InstanceWitnessArgument, InstantiationError> {
    budget.charge()?;
    match witness {
        InstanceWitnessArgument::Concrete(witness) => {
            Ok(InstanceWitnessArgument::Concrete(*witness))
        }
        InstanceWitnessArgument::Parameter(_) => Err(InstantiationError::UnboundWitnessParameter),
        InstanceWitnessArgument::Apply { witness, arguments } => {
            Ok(InstanceWitnessArgument::apply(
                *witness,
                arguments
                    .iter()
                    .map(|argument| clone_concrete_witness(argument, budget))
                    .collect::<Result<Vec<_>, _>>()?,
            ))
        }
    }
}

fn witness_has_parameter(root: &InstanceWitnessArgument) -> bool {
    let mut pending = vec![root];
    while let Some(witness) = pending.pop() {
        match witness {
            InstanceWitnessArgument::Parameter(_) => return true,
            InstanceWitnessArgument::Apply { arguments, .. } => pending.extend(arguments.iter()),
            InstanceWitnessArgument::Concrete(_) => {}
        }
    }
    false
}

fn infer_head_arguments<'target>(
    schema: &Type,
    target: &'target Type,
    arity: u32,
) -> Result<Vec<&'target Type>, InstantiationError> {
    let arity = usize::try_from(arity).map_err(|_| InstantiationError::StructureBudget)?;
    if arity > INSTANCE_KEY_STRUCTURE_BUDGET {
        return Err(InstantiationError::StructureBudget);
    }
    let mut arguments = vec![None; arity];
    let mut pending = vec![(schema, target)];
    let mut visited = 0_usize;
    while let Some((schema, target)) = pending.pop() {
        visited = visited
            .checked_add(1)
            .ok_or(InstantiationError::StructureBudget)?;
        if visited > INSTANCE_KEY_STRUCTURE_BUDGET {
            return Err(InstantiationError::StructureBudget);
        }
        match schema {
            Type::Parameter(index) => {
                let slot = arguments
                    .get_mut(*index as usize)
                    .ok_or(InstantiationError::InvalidCheckedWitnessMetadata)?;
                if slot.is_some_and(|previous| previous != target) {
                    return Err(InstantiationError::InvalidCheckedWitnessMetadata);
                }
                *slot = Some(target);
            }
            Type::Tuple(schema) => {
                let Type::Tuple(target) = target else {
                    return Err(InstantiationError::InvalidCheckedWitnessMetadata);
                };
                push_unification_children(&mut pending, schema, target)?;
            }
            Type::List(schema) => {
                let Type::List(target) = target else {
                    return Err(InstantiationError::InvalidCheckedWitnessMetadata);
                };
                pending.push((schema, target));
            }
            Type::Nominal(schema_id, schema) => {
                let Type::Nominal(target_id, target) = target else {
                    return Err(InstantiationError::InvalidCheckedWitnessMetadata);
                };
                if schema_id != target_id {
                    return Err(InstantiationError::InvalidCheckedWitnessMetadata);
                }
                push_unification_children(&mut pending, schema, target)?;
            }
            Type::Task(schema) => {
                let Type::Task(target) = target else {
                    return Err(InstantiationError::InvalidCheckedWitnessMetadata);
                };
                pending.push((schema, target));
            }
            Type::TaskOutcome(schema) => {
                let Type::TaskOutcome(target) = target else {
                    return Err(InstantiationError::InvalidCheckedWitnessMetadata);
                };
                pending.push((schema, target));
            }
            Type::View {
                mutable: schema_mutable,
                concept: schema_concept,
                bindings: schema_bindings,
            } => {
                let Type::View {
                    mutable: target_mutable,
                    concept: target_concept,
                    bindings: target_bindings,
                } = target
                else {
                    return Err(InstantiationError::InvalidCheckedWitnessMetadata);
                };
                if schema_mutable != target_mutable
                    || schema_concept != target_concept
                    || schema_bindings.keys().ne(target_bindings.keys())
                {
                    return Err(InstantiationError::InvalidCheckedWitnessMetadata);
                }
                pending.extend(schema_bindings.values().zip(target_bindings.values()));
            }
            Type::AssociatedProjection { .. } => {
                return Err(InstantiationError::InvalidCheckedWitnessMetadata);
            }
            Type::Never
            | Type::Unit
            | Type::Bool
            | Type::Int
            | Type::Float
            | Type::Text
            | Type::Error => {
                if schema != target {
                    return Err(InstantiationError::InvalidCheckedWitnessMetadata);
                }
            }
        }
    }
    arguments
        .into_iter()
        .map(|argument| argument.ok_or(InstantiationError::InvalidCheckedWitnessMetadata))
        .collect()
}

fn push_unification_children<'schema, 'target>(
    pending: &mut Vec<(&'schema Type, &'target Type)>,
    schema: &'schema [Type],
    target: &'target [Type],
) -> Result<(), InstantiationError> {
    if schema.len() != target.len() {
        return Err(InstantiationError::InvalidCheckedWitnessMetadata);
    }
    if pending
        .len()
        .checked_add(schema.len())
        .is_none_or(|nodes| nodes > INSTANCE_KEY_STRUCTURE_BUDGET)
    {
        return Err(InstantiationError::StructureBudget);
    }
    pending.extend(schema.iter().zip(target));
    Ok(())
}

fn type_has_open_component(root: &Type) -> bool {
    let mut work = vec![root];
    while let Some(ty) = work.pop() {
        match ty {
            Type::Parameter(_) | Type::AssociatedProjection { .. } => return true,
            Type::Tuple(elements) | Type::Nominal(_, elements) => work.extend(elements),
            Type::List(element) | Type::Task(element) | Type::TaskOutcome(element) => {
                work.push(element);
            }
            Type::View { bindings, .. } => work.extend(bindings.values()),
            Type::Never
            | Type::Unit
            | Type::Bool
            | Type::Int
            | Type::Float
            | Type::Text
            | Type::Error => {}
        }
    }
    false
}

#[derive(Clone)]
struct CallSite {
    key: InstanceKey,
    function: FunctionId,
    expression: ExprId,
    span: Span,
    path: String,
}

struct CallCollector<'plan> {
    remaining: usize,
    calls: Vec<CallSite>,
    dyn_concepts: &'plan DynConceptPlan,
}

impl CallCollector<'_> {
    fn reserve(
        &mut self,
        function: &mir::Function,
        expression: &mir::Expr,
        path: &str,
    ) -> Result<(), InstanceClosureUnsupported> {
        if self.calls.len() >= self.remaining {
            return Err(InstanceClosureUnsupported {
                kind: InstanceClosureUnsupportedKind::InstanceBudget,
                function: function.id,
                expression: Some(expression.id),
                span: expression.span,
                path: format!("{path}.instance"),
            });
        }
        Ok(())
    }
}

enum VisitTask {
    Enter {
        key: InstanceKey,
        site: Option<CallSite>,
    },
    Exit {
        identity: String,
        source: FunctionId,
    },
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum VisitState {
    Visiting,
    Complete,
}

/// Plans the closed set of concrete direct-call instances reachable from the
/// selected run/test roots.
pub(crate) fn plan_instance_closure(
    program: &mir::Program,
    roots: &[FunctionId],
    dyn_concepts: &DynConceptPlan,
) -> Result<InstanceClosureOutcome, InstanceClosureError> {
    let mut tasks = roots
        .iter()
        .rev()
        .map(|root| VisitTask::Enter {
            key: InstanceKey::monomorphic(*root),
            site: None,
        })
        .collect::<Vec<_>>();
    let mut states = BTreeMap::new();
    let mut active_sources = BTreeMap::<FunctionId, String>::new();
    let mut entries = Vec::new();
    let mut instance_calls = BTreeMap::new();
    let mut remaining_call_edges = INSTANCE_CLOSURE_MAX_CALL_EDGES;

    while let Some(task) = tasks.pop() {
        match task {
            VisitTask::Exit { identity, source } => {
                states.insert(identity.clone(), VisitState::Complete);
                if active_sources.get(&source) == Some(&identity) {
                    active_sources.remove(&source);
                }
            }
            VisitTask::Enter { key, site } => {
                let identity = key.canonical_identity();
                if states.contains_key(&identity) {
                    continue;
                }
                if active_sources
                    .get(&key.source())
                    .is_some_and(|active| active != &identity)
                {
                    let site = site.expect("a root cannot recurse into another instance");
                    return Ok(InstanceClosureOutcome::Unsupported(
                        InstanceClosureUnsupported {
                            kind: InstanceClosureUnsupportedKind::NonRegularRecursion,
                            function: site.function,
                            expression: Some(site.expression),
                            span: site.span,
                            path: site.path,
                        },
                    ));
                }
                if entries.len() >= INSTANCE_CLOSURE_MAX_INSTANCES {
                    return Ok(InstanceClosureOutcome::Unsupported(instance_budget_issue(
                        program,
                        &key,
                        site.as_ref(),
                    )?));
                }
                let function = program
                    .function(key.source())
                    .ok_or(InstanceClosureError::MissingFunction(key.source()))?;
                require_instance_arity(function, &key)?;
                let calls = match collect_instance_calls(
                    program,
                    function,
                    &key,
                    remaining_call_edges,
                    dyn_concepts,
                ) {
                    Ok(calls) => calls,
                    Err(CollectCallsError::Unsupported(issue)) => {
                        return Ok(InstanceClosureOutcome::Unsupported(issue));
                    }
                    Err(CollectCallsError::Defect(error)) => return Err(error),
                };
                remaining_call_edges = remaining_call_edges.saturating_sub(calls.len());
                active_sources.insert(key.source(), identity.clone());
                states.insert(identity.clone(), VisitState::Visiting);
                entries.push(key.clone());
                instance_calls.insert(
                    identity.clone(),
                    calls
                        .iter()
                        .map(|site| site.key.clone())
                        .collect::<Vec<_>>()
                        .into_boxed_slice(),
                );
                tasks.push(VisitTask::Exit {
                    identity,
                    source: key.source(),
                });
                tasks.extend(calls.into_iter().rev().map(|site| VisitTask::Enter {
                    key: site.key.clone(),
                    site: Some(site),
                }));
            }
        }
    }

    entries.sort_by(|left, right| {
        left.source()
            .cmp(&right.source())
            .then_with(|| left.canonical_identity().cmp(&right.canonical_identity()))
    });
    Ok(InstanceClosureOutcome::Complete(InstanceClosure {
        entries,
        calls: instance_calls,
    }))
}

fn instance_budget_issue(
    program: &mir::Program,
    key: &InstanceKey,
    site: Option<&CallSite>,
) -> Result<InstanceClosureUnsupported, InstanceClosureError> {
    let (function, expression, span, path) = if let Some(site) = site {
        (
            site.function,
            Some(site.expression),
            site.span,
            site.path.clone(),
        )
    } else {
        let function = program
            .function(key.source())
            .ok_or(InstanceClosureError::MissingFunction(key.source()))?;
        (
            function.id,
            None,
            function.span,
            "artifact.roots".to_owned(),
        )
    };
    Ok(InstanceClosureUnsupported {
        kind: InstanceClosureUnsupportedKind::InstanceBudget,
        function,
        expression,
        span,
        path,
    })
}

fn require_instance_arity(
    function: &mir::Function,
    key: &InstanceKey,
) -> Result<(), InstanceClosureError> {
    let expected_types = function.type_parameters as usize;
    let actual_types = key.type_arguments().len();
    let expected_witnesses = function.witness_params.len();
    let actual_witnesses = key.witness_arguments().len();
    if expected_types == actual_types && expected_witnesses == actual_witnesses {
        return Ok(());
    }
    Err(InstanceClosureError::InvalidInstanceArity {
        function: function.id,
        expected_types,
        actual_types,
        expected_witnesses,
        actual_witnesses,
    })
}

fn collect_instance_calls(
    program: &mir::Program,
    function: &mir::Function,
    key: &InstanceKey,
    remaining: usize,
    dyn_concepts: &DynConceptPlan,
) -> Result<Vec<CallSite>, CollectCallsError> {
    let mut collector = CallCollector {
        remaining,
        calls: Vec::new(),
        dyn_concepts,
    };
    let substitution = InstanceSubstitution::new(program, key);
    let result = scan_block(
        function,
        &function.body,
        &format!("function[{}].body", function.id.0),
        &substitution,
        &mut collector,
    );
    result.map(|_| collector.calls)
}

fn instantiation_issue(
    function: &mir::Function,
    expression: &mir::Expr,
    path: &str,
    error: InstantiationError,
) -> CollectCallsError {
    if error == InstantiationError::InvalidCheckedWitnessMetadata {
        return CollectCallsError::Defect(InstanceClosureError::InvalidCheckedInstantiation {
            function: function.id,
            expression: expression.id,
        });
    }
    CollectCallsError::Unsupported(InstanceClosureUnsupported {
        kind: if error == InstantiationError::StructureBudget {
            InstanceClosureUnsupportedKind::InstanceBudget
        } else {
            InstanceClosureUnsupportedKind::Instantiation
        },
        function: function.id,
        expression: Some(expression.id),
        span: expression.span,
        path: path.to_owned(),
    })
}

enum CollectCallsError {
    Unsupported(InstanceClosureUnsupported),
    Defect(InstanceClosureError),
}

impl From<InstanceClosureUnsupported> for CollectCallsError {
    fn from(issue: InstanceClosureUnsupported) -> Self {
        Self::Unsupported(issue)
    }
}

type ScanResult = Result<bool, CollectCallsError>;

fn scan_block(
    function: &mir::Function,
    block: &mir::Block,
    path: &str,
    substitution: &InstanceSubstitution<'_, '_>,
    calls: &mut CallCollector<'_>,
) -> ScanResult {
    for (index, statement) in block.statements.iter().enumerate() {
        if !scan_statement(
            function,
            statement,
            &format!("{path}.statements[{index}]"),
            substitution,
            calls,
        )? {
            return Ok(false);
        }
    }
    match block.tail.as_deref() {
        Some(tail) => scan_expr(function, tail, &format!("{path}.tail"), substitution, calls),
        None => Ok(true),
    }
}

fn scan_statement(
    function: &mir::Function,
    statement: &mir::Statement,
    path: &str,
    substitution: &InstanceSubstitution<'_, '_>,
    calls: &mut CallCollector<'_>,
) -> ScanResult {
    match &statement.kind {
        StatementKind::Let { value, .. }
        | StatementKind::LetTuple { value, .. }
        | StatementKind::Assign { value, .. }
        | StatementKind::Evaluate(value)
        | StatementKind::Assert { condition: value } => {
            scan_expr(function, value, path, substitution, calls)
        }
        StatementKind::Scoped {
            value, disposal, ..
        } => {
            if !scan_expr(
                function,
                value,
                &format!("{path}.value"),
                substitution,
                calls,
            )? {
                return Ok(false);
            }
            if let mir::ScopedDisposal::StaticConcept {
                requirement,
                witness,
                dispatch_type,
            } = disposal
            {
                let key = substitution
                    .static_call_key(*requirement, witness, dispatch_type, &[], &[])
                    .map_err(|error| {
                        instantiation_issue(function, value, &format!("{path}.disposal"), error)
                    })?;
                calls.reserve(function, value, &format!("{path}.disposal"))?;
                calls.calls.push(CallSite {
                    key,
                    function: function.id,
                    expression: value.id,
                    span: statement.span,
                    path: format!("{path}.disposal.instance"),
                });
            }
            Ok(true)
        }
        StatementKind::ForRange {
            start, end, body, ..
        } => {
            if !scan_expr(
                function,
                start,
                &format!("{path}.start"),
                substitution,
                calls,
            )? || !scan_expr(function, end, &format!("{path}.end"), substitution, calls)?
            {
                return Ok(false);
            }
            let _ = scan_block(function, body, &format!("{path}.body"), substitution, calls)?;
            Ok(true)
        }
        StatementKind::Defer(cleanup) => {
            let _ = scan_block(
                function,
                cleanup,
                &format!("{path}.cleanup"),
                substitution,
                calls,
            )?;
            Ok(true)
        }
        StatementKind::Return(value) => {
            if let Some(value) = value {
                let _ = scan_expr(
                    function,
                    value,
                    &format!("{path}.value"),
                    substitution,
                    calls,
                )?;
            }
            Ok(false)
        }
    }
}

#[allow(clippy::too_many_lines)]
fn scan_expr(
    function: &mir::Function,
    expression: &mir::Expr,
    path: &str,
    substitution: &InstanceSubstitution<'_, '_>,
    calls: &mut CallCollector<'_>,
) -> ScanResult {
    let continues = match &expression.kind {
        ExprKind::Constant(_)
        | ExprKind::Copy(_)
        | ExprKind::Move(_)
        | ExprKind::ReborrowView { .. } => true,
        ExprKind::Tuple(values)
        | ExprKind::List(values)
        | ExprKind::Record { fields: values, .. }
        | ExprKind::Variant {
            payload: values, ..
        }
        | ExprKind::TaskJoin {
            arguments: values, ..
        } => scan_exprs(function, values, path, substitution, calls)?,
        ExprKind::Unary(_, value)
        | ExprKind::Refine { value, .. }
        | ExprKind::Unrefine(value)
        | ExprKind::MakeView { value, .. }
        | ExprKind::Await { task: value, .. }
        | ExprKind::Sleep {
            milliseconds: value,
        } => scan_expr(function, value, path, substitution, calls)?,
        ExprKind::Binary(operator, left, right) => {
            if scan_expr(function, left, &format!("{path}.left"), substitution, calls)? {
                let right = scan_expr(
                    function,
                    right,
                    &format!("{path}.right"),
                    substitution,
                    calls,
                )?;
                right || matches!(operator, mir::BinaryOp::And | mir::BinaryOp::Or)
            } else {
                false
            }
        }
        ExprKind::Block(block) => scan_block(
            function,
            block,
            &format!("{path}.block"),
            substitution,
            calls,
        )?,
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            if scan_expr(
                function,
                condition,
                &format!("{path}.condition"),
                substitution,
                calls,
            )? {
                let then_continues = scan_block(
                    function,
                    then_branch,
                    &format!("{path}.then"),
                    substitution,
                    calls,
                )?;
                let else_continues = scan_block(
                    function,
                    else_branch,
                    &format!("{path}.else"),
                    substitution,
                    calls,
                )?;
                then_continues || else_continues
            } else {
                false
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            if scan_expr(
                function,
                scrutinee,
                &format!("{path}.scrutinee"),
                substitution,
                calls,
            )? {
                let mut continues = false;
                for (index, arm) in arms.iter().enumerate() {
                    continues |= scan_expr(
                        function,
                        &arm.value,
                        &format!("{path}.arms[{index}].value"),
                        substitution,
                        calls,
                    )?;
                }
                continues
            } else {
                false
            }
        }
        ExprKind::Call {
            target,
            type_arguments,
            arguments,
            witnesses,
        } => {
            for (index, argument) in arguments.iter().enumerate() {
                if let CallArgument::Value(value) = argument
                    && !scan_expr(
                        function,
                        value,
                        &format!("{path}.arguments[{index}]"),
                        substitution,
                        calls,
                    )?
                {
                    return Ok(false);
                }
            }
            let key = match target {
                CallTarget::Direct(callee) | CallTarget::Inherent(callee) => Some(
                    substitution
                        .call_key(*callee, type_arguments, witnesses)
                        .map_err(|error| instantiation_issue(function, expression, path, error))?,
                ),
                CallTarget::StaticConcept {
                    requirement,
                    witness,
                    dispatch_type,
                } => Some(
                    substitution
                        .static_call_key(
                            *requirement,
                            witness,
                            dispatch_type,
                            type_arguments,
                            witnesses,
                        )
                        .map_err(|error| instantiation_issue(function, expression, path, error))?,
                ),
                CallTarget::Dynamic { requirement } => {
                    let receiver_ty = arguments
                        .first()
                        .map(|argument| dynamic_receiver_type(function, argument, substitution))
                        .transpose()
                        .map_err(|error| instantiation_issue(function, expression, path, error))?
                        .flatten();
                    receiver_ty
                        .as_ref()
                        .and_then(|receiver_ty| calls.dyn_concepts.choice(receiver_ty))
                        .map(|choice| {
                            substitution.static_call_key(
                                *requirement,
                                &WitnessRef::Concrete(choice.witness()),
                                choice.concrete(),
                                &[],
                                &[],
                            )
                        })
                        .transpose()
                        .map_err(|error| instantiation_issue(function, expression, path, error))?
                }
                CallTarget::Builtin(_) => None,
            };
            if let Some(key) = key {
                calls.reserve(function, expression, path)?;
                calls.calls.push(CallSite {
                    key,
                    function: function.id,
                    expression: expression.id,
                    span: expression.span,
                    path: format!("{path}.instance"),
                });
            }
            expression.ty != Type::Never
        }
    };
    Ok(continues && expression.ty != Type::Never)
}

fn scan_exprs(
    function: &mir::Function,
    expressions: &[mir::Expr],
    path: &str,
    substitution: &InstanceSubstitution<'_, '_>,
    calls: &mut CallCollector<'_>,
) -> ScanResult {
    for (index, expression) in expressions.iter().enumerate() {
        if !scan_expr(
            function,
            expression,
            &format!("{path}[{index}]"),
            substitution,
            calls,
        )? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn dynamic_receiver_type(
    function: &mir::Function,
    argument: &CallArgument,
    substitution: &InstanceSubstitution<'_, '_>,
) -> Result<Option<Type>, InstantiationError> {
    let ty = match argument {
        CallArgument::Value(value) => &value.ty,
        CallArgument::InOut(place) if place.projection.is_empty() => {
            let Some(local) = function
                .params
                .iter()
                .chain(&function.locals)
                .find(|local| local.id == place.local)
            else {
                return Ok(None);
            };
            &local.ty
        }
        CallArgument::InOut(_) => return Ok(None),
    };
    substitution.instantiate_type(ty).map(Some)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use loom_core::Span;
    use loom_mir::{
        Block, CallPlan, ConceptId, Function, FunctionId, Program, RequirementDef, RequirementId,
        RequirementType, Type, Witness, WitnessId, WitnessParam, WitnessRef,
    };

    use super::{InstanceSubstitution, InstantiationError};
    use crate::{INSTANCE_KEY_STRUCTURE_BUDGET, InstanceKey, InstanceWitnessArgument};

    #[test]
    fn substitution_preflights_repeated_expansion_before_cloning() {
        let key = InstanceKey::new(
            FunctionId(0),
            vec![Type::Tuple(vec![Type::Int; 32])],
            Vec::new(),
        );
        let schema = Type::Tuple(vec![Type::Parameter(0); 8]);
        assert_eq!(
            InstanceSubstitution::new(&Program::default(), &key).instantiate_type(&schema),
            Err(InstantiationError::StructureBudget)
        );
    }

    #[test]
    fn forwarded_and_applied_witnesses_keep_static_identity() {
        let key = InstanceKey::new(
            FunctionId(0),
            vec![Type::Int],
            vec![InstanceWitnessArgument::Concrete(WitnessId(7))],
        );
        let call = InstanceSubstitution::new(&Program::default(), &key)
            .call_key(
                FunctionId(1),
                &[Type::Parameter(0)],
                &[WitnessRef::Apply {
                    witness: WitnessId(9),
                    arguments: vec![WitnessRef::Parameter(0)],
                }],
            )
            .expect("bounded closed instance");
        assert_eq!(call.type_arguments(), &[Type::Int]);
        assert_eq!(
            call.witness_arguments(),
            &[InstanceWitnessArgument::apply(
                WitnessId(9),
                vec![InstanceWitnessArgument::Concrete(WitnessId(7))]
            )]
        );
    }

    #[test]
    fn a_maximum_sized_actual_replaces_its_parameter_without_double_counting() {
        let key = InstanceKey::new(
            FunctionId(0),
            vec![Type::Tuple(vec![
                Type::Int;
                INSTANCE_KEY_STRUCTURE_BUDGET - 1
            ])],
            Vec::new(),
        );
        assert!(
            InstanceSubstitution::new(&Program::default(), &key)
                .instantiate_type(&Type::Parameter(0))
                .is_ok()
        );
    }

    #[test]
    fn call_key_preflights_the_combined_type_and_witness_budget() {
        let key = InstanceKey::monomorphic(FunctionId(0));
        let types = [Type::Tuple(vec![Type::Int; 127])];
        let witnesses = [WitnessRef::Apply {
            witness: WitnessId(9),
            arguments: vec![WitnessRef::Concrete(WitnessId(7)); 128],
        }];
        assert_eq!(
            InstanceSubstitution::new(&Program::default(), &key).call_key(
                FunctionId(1),
                &types,
                &witnesses
            ),
            Err(InstantiationError::StructureBudget)
        );
    }

    fn projection_program() -> Program {
        let span = Span::default();
        Program {
            requirements: vec![RequirementDef {
                id: RequirementId(0),
                concept: ConceptId(0),
                name: "item".into(),
                span,
                receiver: None,
                method_type_parameters: 0,
                params: Vec::new(),
                return_ty: RequirementType::Int,
                witness_params: Vec::new(),
            }],
            functions: vec![Function {
                id: FunctionId(0),
                name: "projection.caller".into(),
                span,
                type_parameters: 0,
                is_async: false,
                suspension_points: Vec::new(),
                params: Vec::new(),
                witness_params: vec![WitnessParam {
                    target: Type::Int,
                    concept: ConceptId(0),
                    bindings: BTreeMap::from([("Item".into(), Type::Int)]),
                    span,
                }],
                witness_prefix_count: 0,
                locals: Vec::new(),
                return_ty: Type::AssociatedProjection {
                    witness: 0,
                    associated: "Item".into(),
                },
                receiver: None,
                body: Block {
                    statements: Vec::new(),
                    tail: None,
                    span,
                },
                call_plan: CallPlan::default(),
            }],
            witnesses: vec![Witness {
                id: WitnessId(0),
                concept: ConceptId(0),
                concrete: Type::Int,
                methods: BTreeMap::from([(RequirementId(0), FunctionId(1))]),
                associated: BTreeMap::from([("Item".into(), Type::Int)]),
                type_parameters: 0,
                prerequisites: Vec::new(),
            }],
            ..Program::default()
        }
    }

    #[test]
    fn concrete_associated_projection_and_static_method_resolve_without_runtime_data() {
        let program = projection_program();
        let key = InstanceKey::new(
            FunctionId(0),
            Vec::new(),
            vec![InstanceWitnessArgument::Concrete(WitnessId(0))],
        );
        let substitution = InstanceSubstitution::new(&program, &key);
        assert_eq!(
            substitution
                .instantiate_type(&Type::AssociatedProjection {
                    witness: 0,
                    associated: "Item".into(),
                })
                .expect("normalize associated binding"),
            Type::Int
        );
        assert_eq!(
            substitution
                .static_call_key(
                    RequirementId(0),
                    &WitnessRef::Parameter(0),
                    &Type::Int,
                    &[],
                    &[],
                )
                .expect("resolve static method"),
            InstanceKey::monomorphic(FunctionId(1))
        );
    }

    #[test]
    fn malformed_concrete_proof_is_rejected_instead_of_guessed() {
        let program = projection_program();
        let key = InstanceKey::new(
            FunctionId(0),
            Vec::new(),
            vec![InstanceWitnessArgument::Concrete(WitnessId(9))],
        );
        assert_eq!(
            InstanceSubstitution::new(&program, &key).instantiate_type(
                &Type::AssociatedProjection {
                    witness: 0,
                    associated: "Item".into(),
                },
            ),
            Err(InstantiationError::InvalidCheckedWitnessMetadata)
        );
    }
}
