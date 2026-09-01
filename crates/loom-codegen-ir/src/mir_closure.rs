use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::fmt;

use loom_mir::{
    self as mir, Block, Builtin, CallArgument, CallTarget, ConceptId, Contract, ContractExpr,
    ContractExprKind, Expr, ExprKind, FunctionId, Pattern, PreludeIds, Program, RequirementId,
    RequirementType, ScopedDisposal, StatementKind, Type, TypeDefKind, TypeId, WitnessId,
    WitnessRef,
};

use crate::{GraphError, SourceRoots, analyze_source_reachability};

/// Failure to derive a self-contained interpreted executable from checked MIR.
#[derive(Debug)]
pub enum MirClosureError {
    UnknownEntry { entry: String },
    SourceGraph(GraphError),
    MissingDefinition { kind: &'static str, id: u32 },
    TooManyDefinitions { kind: &'static str },
    InvalidClosedProgram(mir::MirValidationErrors),
}

impl fmt::Display for MirClosureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownEntry { entry } => {
                write!(formatter, "run entry `{entry}` is not exported")
            }
            Self::SourceGraph(error) => {
                write!(formatter, "checked-MIR reachability failed: {error}")
            }
            Self::MissingDefinition { kind, id } => {
                write!(formatter, "closed MIR references missing {kind} #{id}")
            }
            Self::TooManyDefinitions { kind } => {
                write!(formatter, "closed MIR contains too many {kind} definitions")
            }
            Self::InvalidClosedProgram(errors) => {
                write!(formatter, "closed MIR failed validation: {errors}")
            }
        }
    }
}

impl Error for MirClosureError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::SourceGraph(error) => Some(error),
            Self::InvalidClosedProgram(errors) => Some(errors),
            Self::UnknownEntry { .. }
            | Self::MissingDefinition { .. }
            | Self::TooManyDefinitions { .. } => None,
        }
    }
}

/// Closes checked MIR over one fixed executable entry and densely remaps every
/// retained global identity.
///
/// Source reachability supplies the executable seed. A second structural
/// closure retains every reference in each serialized definition, including
/// syntax after a diverging operation, and every method of a retained witness.
/// The latter is required because checked MIR witnesses carry complete concept
/// method tables even though native code generation can emit only live slots.
///
/// The returned program exports only `entry` and contains no test roots. The
/// input remains unchanged and suitable for the full checked-MIR cache.
///
/// # Errors
///
/// Returns [`MirClosureError::UnknownEntry`] when `entry` is not exported, or a
/// compiler-boundary error if reachability, remapping, or final MIR validation
/// fails.
pub fn close_interpreted_executable(
    program: &mir::CheckedProgram,
    entry: &str,
) -> Result<mir::CheckedProgram, MirClosureError> {
    let roots =
        SourceRoots::for_entry(program, entry).ok_or_else(|| MirClosureError::UnknownEntry {
            entry: entry.to_owned(),
        })?;
    let root =
        program
            .exports
            .get(entry)
            .copied()
            .ok_or_else(|| MirClosureError::UnknownEntry {
                entry: entry.to_owned(),
            })?;
    let executable =
        analyze_source_reachability(program, &roots).map_err(MirClosureError::SourceGraph)?;

    let mut closure = SerializationClosure::default();
    for function in executable.functions {
        closure.add_function(function);
    }
    for witness in executable.witnesses {
        closure.add_witness(witness);
    }
    closure.retain_resource_semantic_metadata(program.as_program());
    closure.close(program.as_program())?;

    let maps = IdMaps::new(program.as_program(), &closure)?;
    maps.remap_program(program.as_program(), entry, root, &closure)
}

#[derive(Default)]
struct SerializationClosure {
    live_types: BTreeSet<Type>,
    types: BTreeSet<TypeId>,
    functions: BTreeSet<FunctionId>,
    concepts: BTreeSet<ConceptId>,
    requirements: BTreeSet<RequirementId>,
    witnesses: BTreeSet<WitnessId>,
    type_queue: VecDeque<TypeId>,
    function_queue: VecDeque<FunctionId>,
    concept_queue: VecDeque<ConceptId>,
    requirement_queue: VecDeque<RequirementId>,
    witness_queue: VecDeque<WitnessId>,
}

impl SerializationClosure {
    fn add_type_id(&mut self, id: TypeId) {
        if self.types.insert(id) {
            self.type_queue.push_back(id);
        }
    }

    fn add_function(&mut self, id: FunctionId) {
        if self.functions.insert(id) {
            self.function_queue.push_back(id);
        }
    }

    fn add_concept(&mut self, id: ConceptId) {
        if self.concepts.insert(id) {
            self.concept_queue.push_back(id);
        }
    }

    fn add_requirement(&mut self, id: RequirementId) {
        if self.requirements.insert(id) {
            self.requirement_queue.push_back(id);
        }
    }

    fn add_witness(&mut self, id: WitnessId) {
        if self.witnesses.insert(id) {
            self.witness_queue.push_back(id);
        }
    }

    fn retain_resource_semantic_metadata(&mut self, program: &Program) {
        for concept in [
            program.prelude.dispose_concept,
            program.prelude.must_scope_concept,
            program.prelude.no_suspend_concept,
        ]
        .into_iter()
        .flatten()
        {
            self.add_concept(concept);
        }
        if let Some(requirement) = program.prelude.dispose_requirement {
            self.add_requirement(requirement);
        }
    }

    fn close(&mut self, program: &Program) -> Result<(), MirClosureError> {
        loop {
            if let Some(id) = self.function_queue.pop_front() {
                let function =
                    program
                        .function(id)
                        .cloned()
                        .ok_or(MirClosureError::MissingDefinition {
                            kind: "function",
                            id: id.0,
                        })?;
                self.scan_function(program, &function);
                continue;
            }
            if let Some(id) = self.type_queue.pop_front() {
                let definition =
                    program
                        .type_def(id)
                        .cloned()
                        .ok_or(MirClosureError::MissingDefinition {
                            kind: "type",
                            id: id.0,
                        })?;
                self.scan_type_definition(&definition);
                continue;
            }
            if let Some(id) = self.concept_queue.pop_front() {
                let concept =
                    program
                        .concept(id)
                        .cloned()
                        .ok_or(MirClosureError::MissingDefinition {
                            kind: "concept",
                            id: id.0,
                        })?;
                for requirement in concept.requirements {
                    self.add_requirement(requirement);
                }
                continue;
            }
            if let Some(id) = self.requirement_queue.pop_front() {
                let requirement =
                    program
                        .requirement(id)
                        .cloned()
                        .ok_or(MirClosureError::MissingDefinition {
                            kind: "requirement",
                            id: id.0,
                        })?;
                self.scan_requirement(&requirement);
                continue;
            }
            if let Some(id) = self.witness_queue.pop_front() {
                let witness =
                    program
                        .witness(id)
                        .cloned()
                        .ok_or(MirClosureError::MissingDefinition {
                            kind: "witness",
                            id: id.0,
                        })?;
                self.scan_witness(&witness);
                continue;
            }
            if self.retain_relevant_marker_witnesses(program) {
                continue;
            }
            break;
        }
        Ok(())
    }

    fn retain_relevant_marker_witnesses(&mut self, program: &Program) -> bool {
        let markers = [
            program.prelude.must_scope_concept,
            program.prelude.no_suspend_concept,
        ]
        .into_iter()
        .flatten()
        .collect::<BTreeSet<_>>();
        if markers.is_empty() {
            return false;
        }

        // Marker conformances affect checked-MIR resource validation even
        // without a runtime dispatch edge. Retain only candidates whose head
        // can apply to a serialized value schema. Newly retained witnesses
        // can expose more schemas, so the outer closure loop repeats this to
        // a fixed point.
        let candidates = program
            .witnesses
            .iter()
            .filter(|witness| {
                markers.contains(&witness.concept)
                    && !self.witnesses.contains(&witness.id)
                    && (self.type_head_is_live(&witness.concrete)
                        || self
                            .live_types
                            .iter()
                            .any(|live| type_heads_may_unify(&witness.concrete, live)))
            })
            .map(|witness| witness.id)
            .collect::<Vec<_>>();
        let changed = !candidates.is_empty();
        for witness in candidates {
            self.add_witness(witness);
        }
        changed
    }

    fn type_head_is_live(&self, ty: &Type) -> bool {
        match ty {
            Type::Nominal(id, _) => self.types.contains(id),
            Type::Never
            | Type::Unit
            | Type::Bool
            | Type::Int
            | Type::Float
            | Type::Text
            | Type::Tuple(_)
            | Type::List(_)
            | Type::Parameter(_)
            | Type::AssociatedProjection { .. }
            | Type::Task(_)
            | Type::TaskOutcome(_)
            | Type::View { .. }
            | Type::Error => false,
        }
    }

    fn scan_type(&mut self, ty: &Type) {
        self.live_types.insert(ty.clone());
        match ty {
            Type::Tuple(elements) => {
                for element in elements {
                    self.scan_type(element);
                }
            }
            Type::List(element) | Type::Task(element) | Type::TaskOutcome(element) => {
                self.scan_type(element);
            }
            Type::Nominal(id, arguments) => {
                self.add_type_id(*id);
                for argument in arguments {
                    self.scan_type(argument);
                }
            }
            Type::View {
                concept, bindings, ..
            } => {
                self.add_concept(*concept);
                for binding in bindings.values() {
                    self.scan_type(binding);
                }
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

    fn scan_requirement_type(&mut self, ty: &RequirementType) {
        match ty {
            RequirementType::Tuple(elements) => {
                for element in elements {
                    self.scan_requirement_type(element);
                }
            }
            RequirementType::Nominal(id, arguments) => {
                self.add_type_id(*id);
                for argument in arguments {
                    self.scan_requirement_type(argument);
                }
            }
            RequirementType::View {
                concept, bindings, ..
            } => {
                self.add_concept(*concept);
                for binding in bindings.values() {
                    self.scan_requirement_type(binding);
                }
            }
            RequirementType::Unit
            | RequirementType::Bool
            | RequirementType::Int
            | RequirementType::Float
            | RequirementType::Text
            | RequirementType::SelfType
            | RequirementType::Associated(_)
            | RequirementType::MethodParameter(_)
            | RequirementType::AssociatedProjection { .. } => {}
        }
    }

    fn scan_type_definition(&mut self, definition: &mir::TypeDef) {
        match &definition.kind {
            TypeDefKind::Record { fields, invariant } => {
                for field in fields {
                    self.scan_type(&field.ty);
                }
                if let Some(invariant) = invariant {
                    self.scan_contract(invariant);
                }
            }
            TypeDefKind::Enum { variants } => {
                for payload in variants.iter().flat_map(|variant| &variant.payload) {
                    self.scan_type(payload);
                }
            }
            TypeDefKind::Refined { base, predicate } => {
                self.scan_type(base);
                self.scan_contract(predicate);
            }
        }
    }

    fn scan_function(&mut self, program: &Program, function: &mir::Function) {
        for local in function.params.iter().chain(&function.locals) {
            self.scan_type(&local.ty);
        }
        for witness in &function.witness_params {
            self.scan_type(&witness.target);
            self.add_concept(witness.concept);
            for binding in witness.bindings.values() {
                self.scan_type(binding);
            }
        }
        self.scan_type(&function.return_ty);
        if let Some(contract) = &function.call_plan.receiver_invariant {
            self.scan_contract(contract);
        }
        for contract in function
            .call_plan
            .requires
            .iter()
            .chain(&function.call_plan.ensures)
        {
            self.scan_contract(contract);
        }
        self.scan_block(program, &function.body);
    }

    fn scan_requirement(&mut self, requirement: &mir::RequirementDef) {
        self.add_concept(requirement.concept);
        for parameter in &requirement.params {
            self.scan_requirement_type(parameter);
        }
        self.scan_requirement_type(&requirement.return_ty);
        for witness in &requirement.witness_params {
            self.scan_requirement_type(&witness.target);
            self.add_concept(witness.concept);
            for binding in witness.bindings.values() {
                self.scan_requirement_type(binding);
            }
        }
    }

    fn scan_witness(&mut self, witness: &mir::Witness) {
        self.add_concept(witness.concept);
        self.scan_type(&witness.concrete);
        for associated in witness.associated.values() {
            self.scan_type(associated);
        }
        for prerequisite in &witness.prerequisites {
            self.scan_type(&prerequisite.target);
            self.add_concept(prerequisite.concept);
            for binding in prerequisite.bindings.values() {
                self.scan_type(binding);
            }
        }
        for (requirement, function) in &witness.methods {
            self.add_requirement(*requirement);
            self.add_function(*function);
        }
    }

    fn scan_contract(&mut self, contract: &Contract) {
        self.scan_contract_expr(&contract.expression);
    }

    fn scan_contract_expr(&mut self, expression: &ContractExpr) {
        match &expression.kind {
            ContractExprKind::Field(value, _) | ContractExprKind::Unary(_, value) => {
                self.scan_contract_expr(value);
            }
            ContractExprKind::Binary(_, left, right) => {
                self.scan_contract_expr(left);
                self.scan_contract_expr(right);
            }
            ContractExprKind::IsFinite(value) => self.scan_contract_expr(value),
            ContractExprKind::Match { scrutinee, arms } => {
                self.scan_contract_expr(scrutinee);
                for arm in arms {
                    self.scan_pattern(&arm.pattern);
                    for binding in &arm.bindings {
                        self.scan_type(binding);
                    }
                    self.scan_contract_expr(&arm.value);
                }
            }
            ContractExprKind::Constant(_)
            | ContractExprKind::Value(_)
            | ContractExprKind::Binding(_) => {}
        }
    }

    fn scan_pattern(&mut self, pattern: &Pattern) {
        if let Pattern::Variant { ty, payload, .. } = pattern {
            self.add_type_id(*ty);
            for pattern in payload {
                self.scan_pattern(pattern);
            }
        }
    }

    fn scan_block(&mut self, program: &Program, block: &Block) {
        for statement in &block.statements {
            match &statement.kind {
                StatementKind::Let { value, .. }
                | StatementKind::LetTuple { value, .. }
                | StatementKind::Assign { value, .. }
                | StatementKind::Evaluate(value) => self.scan_expr(program, value),
                StatementKind::Scoped {
                    value, disposal, ..
                } => {
                    self.scan_expr(program, value);
                    let ScopedDisposal::StaticConcept {
                        requirement,
                        witness,
                        dispatch_type,
                    } = disposal;
                    self.add_requirement(*requirement);
                    self.scan_witness_ref(witness);
                    self.scan_type(dispatch_type);
                }
                StatementKind::ForRange {
                    start, end, body, ..
                } => {
                    self.scan_expr(program, start);
                    self.scan_expr(program, end);
                    self.scan_block(program, body);
                }
                StatementKind::While { condition, body } => {
                    self.scan_expr(program, condition);
                    self.scan_block(program, body);
                }
                StatementKind::Break
                | StatementKind::Continue
                | StatementKind::RestoreReceiverInvariant { .. } => {}
                StatementKind::Assert { condition } => self.scan_expr(program, condition),
                StatementKind::Defer(cleanup) => self.scan_block(program, cleanup),
                StatementKind::Return(value) => {
                    if let Some(value) = value {
                        self.scan_expr(program, value);
                    }
                }
            }
        }
        if let Some(tail) = &block.tail {
            self.scan_expr(program, tail);
        }
    }

    #[allow(clippy::too_many_lines)]
    fn scan_expr(&mut self, program: &Program, expression: &Expr) {
        self.scan_type(&expression.ty);
        match &expression.kind {
            ExprKind::Tuple(elements) | ExprKind::List(elements) => {
                for element in elements {
                    self.scan_expr(program, element);
                }
            }
            ExprKind::Unary(_, value)
            | ExprKind::Unrefine(value)
            | ExprKind::Await { task: value, .. }
            | ExprKind::Sleep {
                milliseconds: value,
            } => self.scan_expr(program, value),
            ExprKind::Binary(_, left, right) => {
                self.scan_expr(program, left);
                self.scan_expr(program, right);
            }
            ExprKind::Block(block) => self.scan_block(program, block),
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.scan_expr(program, condition);
                self.scan_block(program, then_branch);
                self.scan_block(program, else_branch);
            }
            ExprKind::Match { scrutinee, arms } => {
                self.scan_expr(program, scrutinee);
                for arm in arms {
                    self.scan_pattern(&arm.pattern);
                    self.scan_expr(program, &arm.value);
                }
            }
            ExprKind::Record {
                ty,
                type_arguments,
                fields,
                ..
            }
            | ExprKind::Variant {
                ty,
                type_arguments,
                payload: fields,
                ..
            } => {
                self.add_type_id(*ty);
                for argument in type_arguments {
                    self.scan_type(argument);
                }
                for field in fields {
                    self.scan_expr(program, field);
                }
            }
            ExprKind::Refine { ty, value, .. } => {
                self.add_type_id(*ty);
                self.scan_expr(program, value);
            }
            ExprKind::Call {
                target,
                type_arguments,
                arguments,
                witnesses,
            } => {
                match target {
                    CallTarget::Direct(function) | CallTarget::Inherent(function) => {
                        self.add_function(*function);
                    }
                    CallTarget::StaticConcept {
                        requirement,
                        witness,
                        dispatch_type,
                    } => {
                        self.add_requirement(*requirement);
                        self.scan_witness_ref(witness);
                        self.scan_type(dispatch_type);
                    }
                    CallTarget::Dynamic { requirement } => {
                        self.add_requirement(*requirement);
                    }
                    CallTarget::Builtin(builtin) => {
                        self.retain_builtin_prelude(program, *builtin);
                    }
                }
                for argument in type_arguments {
                    self.scan_type(argument);
                }
                for argument in arguments {
                    if let CallArgument::Value(value) = argument {
                        self.scan_expr(program, value);
                    }
                }
                for witness in witnesses {
                    self.scan_witness_ref(witness);
                }
            }
            ExprKind::MakeView { value, witness, .. } => {
                self.scan_expr(program, value);
                self.scan_witness_ref(witness);
            }
            ExprKind::TaskJoin { arguments, .. } => {
                for argument in arguments {
                    self.scan_expr(program, argument);
                }
            }
            ExprKind::Constant(_)
            | ExprKind::Copy(_)
            | ExprKind::Move(_)
            | ExprKind::ReborrowView { .. } => {}
        }
    }

    fn scan_witness_ref(&mut self, witness: &WitnessRef) {
        match witness {
            WitnessRef::Concrete(witness) => self.add_witness(*witness),
            WitnessRef::Parameter(_) => {}
            WitnessRef::Apply { witness, arguments } => {
                self.add_witness(*witness);
                for argument in arguments {
                    self.scan_witness_ref(argument);
                }
            }
        }
    }

    fn add_optional_type(&mut self, id: Option<TypeId>) {
        if let Some(id) = id {
            self.add_type_id(id);
        }
    }

    #[allow(clippy::too_many_lines)]
    fn retain_builtin_prelude(&mut self, program: &Program, builtin: Builtin) {
        let prelude = program.prelude;
        match builtin {
            Builtin::ProcessEnvironment
            | Builtin::TextGet
            | Builtin::BytesGet
            | Builtin::ListGet => {
                self.add_optional_type(prelude.option);
            }
            Builtin::TextEncodeUtf8
            | Builtin::BytesLength
            | Builtin::BytesAdd
            | Builtin::BytesAppend => {
                self.add_optional_type(prelude.bytes);
            }
            Builtin::BytesDecodeUtf8 => {
                self.add_optional_type(prelude.bytes);
                self.add_optional_type(prelude.result);
                self.add_optional_type(prelude.decode_text_error);
            }
            Builtin::PathFromText | Builtin::PathJoin => {
                self.add_optional_type(prelude.path);
                self.add_optional_type(prelude.result);
                self.add_optional_type(prelude.path_error);
            }
            Builtin::PathAsText => self.add_optional_type(prelude.path),
            Builtin::TaskFaultCode | Builtin::TaskFaultMessage => {
                self.add_optional_type(prelude.task_fault);
            }
            Builtin::FileOpenRead
            | Builtin::FileCreate
            | Builtin::FileReadText
            | Builtin::FileWriteText
            | Builtin::FileClose => {
                self.add_optional_type(prelude.file);
            }
            Builtin::FileTryOpenRead
            | Builtin::FileTryCreate
            | Builtin::FileTryReadText
            | Builtin::FileTryWriteText => {
                self.add_optional_type(prelude.file);
                self.add_optional_type(prelude.result);
                self.add_optional_type(prelude.io_error);
            }
            Builtin::SocketConnect
            | Builtin::SocketReadText
            | Builtin::SocketWriteText
            | Builtin::SocketClose => {
                self.add_optional_type(prelude.socket);
            }
            Builtin::SocketTryConnect
            | Builtin::SocketTryReadText
            | Builtin::SocketTryWriteText => {
                self.add_optional_type(prelude.socket);
                self.add_optional_type(prelude.result);
                self.add_optional_type(prelude.io_error);
            }
            Builtin::TextMapNew
            | Builtin::TextMapLength
            | Builtin::TextMapContains
            | Builtin::TextMapInsert
            | Builtin::TextMapRemove => {
                self.add_optional_type(prelude.text_map);
            }
            Builtin::ListToTextMap => {
                self.add_optional_type(prelude.text_map);
                self.add_optional_type(prelude.result);
            }
            Builtin::TextMapGet | Builtin::TextMapEntryAt => {
                self.add_optional_type(prelude.text_map);
                self.add_optional_type(prelude.option);
            }
            Builtin::LogWrite => {
                self.add_optional_type(prelude.log_level);
                self.add_optional_type(prelude.text_map);
            }
            Builtin::StdoutWrite
            | Builtin::FloatParseStatus
            | Builtin::IntToFloat
            | Builtin::FloatToIntStatus
            | Builtin::FloatFormat
            | Builtin::TextLength
            | Builtin::TextConcat
            | Builtin::TextContains
            | Builtin::ListAdd
            | Builtin::ListLength
            | Builtin::ProcessArgumentCount
            | Builtin::ProcessArgumentAt => {}
        }
    }
}

fn type_heads_may_unify(pattern: &Type, live: &Type) -> bool {
    match (pattern, live) {
        (Type::Tuple(pattern), Type::Tuple(live)) => {
            pattern.len() == live.len()
                && pattern
                    .iter()
                    .zip(live)
                    .all(|(pattern, live)| type_heads_may_unify(pattern, live))
        }
        (Type::List(pattern), Type::List(live))
        | (Type::Task(pattern), Type::Task(live))
        | (Type::TaskOutcome(pattern), Type::TaskOutcome(live)) => {
            type_heads_may_unify(pattern, live)
        }
        (Type::Nominal(pattern_id, pattern), Type::Nominal(live_id, live)) => {
            pattern_id == live_id
                && pattern.len() == live.len()
                && pattern
                    .iter()
                    .zip(live)
                    .all(|(pattern, live)| type_heads_may_unify(pattern, live))
        }
        (
            Type::View {
                concept: pattern, ..
            },
            Type::View { concept: live, .. },
        ) => pattern == live,
        (Type::Parameter(_), _)
        | (Type::Never, Type::Never)
        | (Type::Unit, Type::Unit)
        | (Type::Bool, Type::Bool)
        | (Type::Int, Type::Int)
        | (Type::Float, Type::Float)
        | (Type::Text, Type::Text)
        | (Type::Error, Type::Error) => true,
        (
            Type::AssociatedProjection {
                witness: pattern_witness,
                associated: pattern_associated,
            },
            Type::AssociatedProjection {
                witness: live_witness,
                associated: live_associated,
            },
        ) => pattern_witness == live_witness && pattern_associated == live_associated,
        _ => false,
    }
}

struct IdMaps {
    types: Vec<Option<TypeId>>,
    functions: Vec<Option<FunctionId>>,
    concepts: Vec<Option<ConceptId>>,
    requirements: Vec<Option<RequirementId>>,
    witnesses: Vec<Option<WitnessId>>,
}

impl IdMaps {
    fn new(program: &Program, closure: &SerializationClosure) -> Result<Self, MirClosureError> {
        Ok(Self {
            types: dense_id_map(program.types.len(), &closure.types, TypeId, "type")?,
            functions: dense_id_map(
                program.functions.len(),
                &closure.functions,
                FunctionId,
                "function",
            )?,
            concepts: dense_id_map(
                program.concepts.len(),
                &closure.concepts,
                ConceptId,
                "concept",
            )?,
            requirements: dense_id_map(
                program.requirements.len(),
                &closure.requirements,
                RequirementId,
                "requirement",
            )?,
            witnesses: dense_id_map(
                program.witnesses.len(),
                &closure.witnesses,
                WitnessId,
                "witness",
            )?,
        })
    }

    fn remap_program(
        &self,
        program: &Program,
        entry: &str,
        root: FunctionId,
        closure: &SerializationClosure,
    ) -> Result<mir::CheckedProgram, MirClosureError> {
        let mut types = Vec::with_capacity(closure.types.len());
        for definition in &program.types {
            if closure.types.contains(&definition.id) {
                let mut definition = definition.clone();
                self.remap_type_definition(&mut definition)?;
                types.push(definition);
            }
        }

        let mut concepts = Vec::with_capacity(closure.concepts.len());
        for concept in &program.concepts {
            if closure.concepts.contains(&concept.id) {
                let mut concept = concept.clone();
                concept.id = self.concept(concept.id)?;
                for requirement in &mut concept.requirements {
                    *requirement = self.requirement(*requirement)?;
                }
                concepts.push(concept);
            }
        }

        let mut requirements = Vec::with_capacity(closure.requirements.len());
        for requirement in &program.requirements {
            if closure.requirements.contains(&requirement.id) {
                let mut requirement = requirement.clone();
                self.remap_requirement(&mut requirement)?;
                requirements.push(requirement);
            }
        }

        let mut functions = Vec::with_capacity(closure.functions.len());
        for function in &program.functions {
            if closure.functions.contains(&function.id) {
                let mut function = function.clone();
                self.remap_function(&mut function)?;
                functions.push(function);
            }
        }

        let mut witnesses = Vec::with_capacity(closure.witnesses.len());
        for witness in &program.witnesses {
            if closure.witnesses.contains(&witness.id) {
                let mut witness = witness.clone();
                self.remap_witness(&mut witness)?;
                witnesses.push(witness);
            }
        }

        let closed = Program {
            types,
            concepts,
            requirements,
            functions,
            witnesses,
            tests: Vec::new(),
            exports: BTreeMap::from([(entry.to_owned(), self.function(root)?)]),
            prelude: self.remap_prelude(program.prelude),
        };
        mir::check_program(closed).map_err(MirClosureError::InvalidClosedProgram)
    }

    fn remap_type_definition(&self, definition: &mut mir::TypeDef) -> Result<(), MirClosureError> {
        definition.id = self.ty(definition.id)?;
        match &mut definition.kind {
            TypeDefKind::Record { fields, invariant } => {
                for field in fields {
                    self.remap_type(&mut field.ty)?;
                }
                if let Some(invariant) = invariant {
                    self.remap_contract(invariant)?;
                }
            }
            TypeDefKind::Enum { variants } => {
                for payload in variants.iter_mut().flat_map(|variant| &mut variant.payload) {
                    self.remap_type(payload)?;
                }
            }
            TypeDefKind::Refined { base, predicate } => {
                self.remap_type(base)?;
                self.remap_contract(predicate)?;
            }
        }
        Ok(())
    }

    fn remap_requirement(
        &self,
        requirement: &mut mir::RequirementDef,
    ) -> Result<(), MirClosureError> {
        requirement.id = self.requirement(requirement.id)?;
        requirement.concept = self.concept(requirement.concept)?;
        for parameter in &mut requirement.params {
            self.remap_requirement_type(parameter)?;
        }
        self.remap_requirement_type(&mut requirement.return_ty)?;
        for witness in &mut requirement.witness_params {
            self.remap_requirement_type(&mut witness.target)?;
            witness.concept = self.concept(witness.concept)?;
            for binding in witness.bindings.values_mut() {
                self.remap_requirement_type(binding)?;
            }
        }
        Ok(())
    }

    fn remap_function(&self, function: &mut mir::Function) -> Result<(), MirClosureError> {
        function.id = self.function(function.id)?;
        for local in function.params.iter_mut().chain(&mut function.locals) {
            self.remap_type(&mut local.ty)?;
        }
        for witness in &mut function.witness_params {
            self.remap_type(&mut witness.target)?;
            witness.concept = self.concept(witness.concept)?;
            for binding in witness.bindings.values_mut() {
                self.remap_type(binding)?;
            }
        }
        self.remap_type(&mut function.return_ty)?;
        if let Some(contract) = &mut function.call_plan.receiver_invariant {
            self.remap_contract(contract)?;
        }
        for contract in function
            .call_plan
            .requires
            .iter_mut()
            .chain(&mut function.call_plan.ensures)
        {
            self.remap_contract(contract)?;
        }
        self.remap_block(&mut function.body)
    }

    fn remap_witness(&self, witness: &mut mir::Witness) -> Result<(), MirClosureError> {
        witness.id = self.witness(witness.id)?;
        witness.concept = self.concept(witness.concept)?;
        self.remap_type(&mut witness.concrete)?;
        for associated in witness.associated.values_mut() {
            self.remap_type(associated)?;
        }
        for prerequisite in &mut witness.prerequisites {
            self.remap_type(&mut prerequisite.target)?;
            prerequisite.concept = self.concept(prerequisite.concept)?;
            for binding in prerequisite.bindings.values_mut() {
                self.remap_type(binding)?;
            }
        }
        let methods = std::mem::take(&mut witness.methods);
        for (requirement, function) in methods {
            witness
                .methods
                .insert(self.requirement(requirement)?, self.function(function)?);
        }
        Ok(())
    }

    fn remap_type(&self, ty: &mut Type) -> Result<(), MirClosureError> {
        match ty {
            Type::Tuple(elements) => {
                for element in elements {
                    self.remap_type(element)?;
                }
            }
            Type::List(element) | Type::Task(element) | Type::TaskOutcome(element) => {
                self.remap_type(element)?;
            }
            Type::Nominal(id, arguments) => {
                *id = self.ty(*id)?;
                for argument in arguments {
                    self.remap_type(argument)?;
                }
            }
            Type::View {
                concept, bindings, ..
            } => {
                *concept = self.concept(*concept)?;
                for binding in bindings.values_mut() {
                    self.remap_type(binding)?;
                }
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
        Ok(())
    }

    fn remap_requirement_type(&self, ty: &mut RequirementType) -> Result<(), MirClosureError> {
        match ty {
            RequirementType::Tuple(elements) => {
                for element in elements {
                    self.remap_requirement_type(element)?;
                }
            }
            RequirementType::Nominal(id, arguments) => {
                *id = self.ty(*id)?;
                for argument in arguments {
                    self.remap_requirement_type(argument)?;
                }
            }
            RequirementType::View {
                concept, bindings, ..
            } => {
                *concept = self.concept(*concept)?;
                for binding in bindings.values_mut() {
                    self.remap_requirement_type(binding)?;
                }
            }
            RequirementType::Unit
            | RequirementType::Bool
            | RequirementType::Int
            | RequirementType::Float
            | RequirementType::Text
            | RequirementType::SelfType
            | RequirementType::Associated(_)
            | RequirementType::MethodParameter(_)
            | RequirementType::AssociatedProjection { .. } => {}
        }
        Ok(())
    }

    fn remap_contract(&self, contract: &mut Contract) -> Result<(), MirClosureError> {
        self.remap_contract_expr(&mut contract.expression)
    }

    fn remap_contract_expr(&self, expression: &mut ContractExpr) -> Result<(), MirClosureError> {
        match &mut expression.kind {
            ContractExprKind::Field(value, _) | ContractExprKind::Unary(_, value) => {
                self.remap_contract_expr(value)?;
            }
            ContractExprKind::Binary(_, left, right) => {
                self.remap_contract_expr(left)?;
                self.remap_contract_expr(right)?;
            }
            ContractExprKind::IsFinite(value) => self.remap_contract_expr(value)?,
            ContractExprKind::Match { scrutinee, arms } => {
                self.remap_contract_expr(scrutinee)?;
                for arm in arms {
                    self.remap_pattern(&mut arm.pattern)?;
                    for binding in &mut arm.bindings {
                        self.remap_type(binding)?;
                    }
                    self.remap_contract_expr(&mut arm.value)?;
                }
            }
            ContractExprKind::Constant(_)
            | ContractExprKind::Value(_)
            | ContractExprKind::Binding(_) => {}
        }
        Ok(())
    }

    fn remap_pattern(&self, pattern: &mut Pattern) -> Result<(), MirClosureError> {
        if let Pattern::Variant { ty, payload, .. } = pattern {
            *ty = self.ty(*ty)?;
            for pattern in payload {
                self.remap_pattern(pattern)?;
            }
        }
        Ok(())
    }

    fn remap_block(&self, block: &mut Block) -> Result<(), MirClosureError> {
        for statement in &mut block.statements {
            match &mut statement.kind {
                StatementKind::Let { value, .. }
                | StatementKind::LetTuple { value, .. }
                | StatementKind::Assign { value, .. }
                | StatementKind::Evaluate(value) => self.remap_expr(value)?,
                StatementKind::Scoped {
                    value, disposal, ..
                } => {
                    self.remap_expr(value)?;
                    let ScopedDisposal::StaticConcept {
                        requirement,
                        witness,
                        dispatch_type,
                    } = disposal;
                    *requirement = self.requirement(*requirement)?;
                    self.remap_witness_ref(witness)?;
                    self.remap_type(dispatch_type)?;
                }
                StatementKind::ForRange {
                    start, end, body, ..
                } => {
                    self.remap_expr(start)?;
                    self.remap_expr(end)?;
                    self.remap_block(body)?;
                }
                StatementKind::While { condition, body } => {
                    self.remap_expr(condition)?;
                    self.remap_block(body)?;
                }
                StatementKind::Break
                | StatementKind::Continue
                | StatementKind::RestoreReceiverInvariant { .. } => {}
                StatementKind::Assert { condition } => self.remap_expr(condition)?,
                StatementKind::Defer(cleanup) => self.remap_block(cleanup)?,
                StatementKind::Return(value) => {
                    if let Some(value) = value {
                        self.remap_expr(value)?;
                    }
                }
            }
        }
        if let Some(tail) = &mut block.tail {
            self.remap_expr(tail)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn remap_expr(&self, expression: &mut Expr) -> Result<(), MirClosureError> {
        match &mut expression.kind {
            ExprKind::Tuple(elements) | ExprKind::List(elements) => {
                for element in elements {
                    self.remap_expr(element)?;
                }
            }
            ExprKind::Unary(_, value)
            | ExprKind::Unrefine(value)
            | ExprKind::Await { task: value, .. }
            | ExprKind::Sleep {
                milliseconds: value,
            } => self.remap_expr(value)?,
            ExprKind::Binary(_, left, right) => {
                self.remap_expr(left)?;
                self.remap_expr(right)?;
            }
            ExprKind::Block(block) => self.remap_block(block)?,
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.remap_expr(condition)?;
                self.remap_block(then_branch)?;
                self.remap_block(else_branch)?;
            }
            ExprKind::Match { scrutinee, arms } => {
                self.remap_expr(scrutinee)?;
                for arm in arms {
                    self.remap_pattern(&mut arm.pattern)?;
                    self.remap_expr(&mut arm.value)?;
                }
            }
            ExprKind::Record {
                ty,
                type_arguments,
                fields,
                ..
            }
            | ExprKind::Variant {
                ty,
                type_arguments,
                payload: fields,
                ..
            } => {
                *ty = self.ty(*ty)?;
                for argument in type_arguments {
                    self.remap_type(argument)?;
                }
                for field in fields {
                    self.remap_expr(field)?;
                }
            }
            ExprKind::Refine { ty, value, .. } => {
                *ty = self.ty(*ty)?;
                self.remap_expr(value)?;
            }
            ExprKind::Call {
                target,
                type_arguments,
                arguments,
                witnesses,
            } => {
                match target {
                    CallTarget::Direct(function) | CallTarget::Inherent(function) => {
                        *function = self.function(*function)?;
                    }
                    CallTarget::StaticConcept {
                        requirement,
                        witness,
                        dispatch_type,
                    } => {
                        *requirement = self.requirement(*requirement)?;
                        self.remap_witness_ref(witness)?;
                        self.remap_type(dispatch_type)?;
                    }
                    CallTarget::Dynamic { requirement } => {
                        *requirement = self.requirement(*requirement)?;
                    }
                    CallTarget::Builtin(_) => {}
                }
                for argument in type_arguments {
                    self.remap_type(argument)?;
                }
                for argument in arguments {
                    if let CallArgument::Value(value) = argument {
                        self.remap_expr(value)?;
                    }
                }
                for witness in witnesses {
                    self.remap_witness_ref(witness)?;
                }
            }
            ExprKind::MakeView { value, witness, .. } => {
                self.remap_expr(value)?;
                self.remap_witness_ref(witness)?;
            }
            ExprKind::TaskJoin { arguments, .. } => {
                for argument in arguments {
                    self.remap_expr(argument)?;
                }
            }
            ExprKind::Constant(_)
            | ExprKind::Copy(_)
            | ExprKind::Move(_)
            | ExprKind::ReborrowView { .. } => {}
        }
        self.remap_type(&mut expression.ty)
    }

    fn remap_witness_ref(&self, witness: &mut WitnessRef) -> Result<(), MirClosureError> {
        match witness {
            WitnessRef::Concrete(witness) => *witness = self.witness(*witness)?,
            WitnessRef::Parameter(_) => {}
            WitnessRef::Apply { witness, arguments } => {
                *witness = self.witness(*witness)?;
                for argument in arguments {
                    self.remap_witness_ref(argument)?;
                }
            }
        }
        Ok(())
    }

    fn remap_prelude(&self, mut prelude: PreludeIds) -> PreludeIds {
        prelude.result = self.optional_type(prelude.result);
        prelude.option = self.optional_type(prelude.option);
        prelude.constraint_error = self.optional_type(prelude.constraint_error);
        prelude.task_fault = self.optional_type(prelude.task_fault);
        prelude.task_outcome = self.optional_type(prelude.task_outcome);
        prelude.file = self.optional_type(prelude.file);
        prelude.socket = self.optional_type(prelude.socket);
        prelude.bytes = self.optional_type(prelude.bytes);
        prelude.path = self.optional_type(prelude.path);
        prelude.decode_text_error = self.optional_type(prelude.decode_text_error);
        prelude.path_error = self.optional_type(prelude.path_error);
        prelude.text_map = self.optional_type(prelude.text_map);
        prelude.io_error = self.optional_type(prelude.io_error);
        prelude.io_error_kind = self.optional_type(prelude.io_error_kind);
        prelude.log_level = self.optional_type(prelude.log_level);
        prelude.dispose_concept = self.optional_concept(prelude.dispose_concept);
        prelude.dispose_requirement = self.optional_requirement(prelude.dispose_requirement);
        prelude.must_scope_concept = self.optional_concept(prelude.must_scope_concept);
        prelude.no_suspend_concept = self.optional_concept(prelude.no_suspend_concept);
        prelude
    }

    fn ty(&self, id: TypeId) -> Result<TypeId, MirClosureError> {
        mapped_id(&self.types, id.0, "type")
    }

    fn function(&self, id: FunctionId) -> Result<FunctionId, MirClosureError> {
        mapped_id(&self.functions, id.0, "function")
    }

    fn concept(&self, id: ConceptId) -> Result<ConceptId, MirClosureError> {
        mapped_id(&self.concepts, id.0, "concept")
    }

    fn requirement(&self, id: RequirementId) -> Result<RequirementId, MirClosureError> {
        mapped_id(&self.requirements, id.0, "requirement")
    }

    fn witness(&self, id: WitnessId) -> Result<WitnessId, MirClosureError> {
        mapped_id(&self.witnesses, id.0, "witness")
    }

    fn optional_type(&self, id: Option<TypeId>) -> Option<TypeId> {
        id.and_then(|id| self.types.get(id.0 as usize).copied().flatten())
    }

    fn optional_concept(&self, id: Option<ConceptId>) -> Option<ConceptId> {
        id.and_then(|id| self.concepts.get(id.0 as usize).copied().flatten())
    }

    fn optional_requirement(&self, id: Option<RequirementId>) -> Option<RequirementId> {
        id.and_then(|id| self.requirements.get(id.0 as usize).copied().flatten())
    }
}

fn dense_id_map<T: Copy + Ord>(
    len: usize,
    retained: &BTreeSet<T>,
    make: impl Fn(u32) -> T,
    kind: &'static str,
) -> Result<Vec<Option<T>>, MirClosureError> {
    let mut map = vec![None; len];
    let mut next = 0_usize;
    for (old, slot) in map.iter_mut().enumerate() {
        let old = u32::try_from(old).map_err(|_| MirClosureError::TooManyDefinitions { kind })?;
        if retained.contains(&make(old)) {
            let new =
                u32::try_from(next).map_err(|_| MirClosureError::TooManyDefinitions { kind })?;
            *slot = Some(make(new));
            next += 1;
        }
    }
    Ok(map)
}

fn mapped_id<T: Copy>(
    map: &[Option<T>],
    id: u32,
    kind: &'static str,
) -> Result<T, MirClosureError> {
    map.get(id as usize)
        .copied()
        .flatten()
        .ok_or(MirClosureError::MissingDefinition { kind, id })
}
