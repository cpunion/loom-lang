use std::collections::VecDeque;
use std::error::Error;
use std::fmt;

use loom_mir::Type;

use crate::ids::ProgramBrand;
use crate::{
    CheckedProgram, Function, InstanceId, InstructionKind, Program, RepresentationPlan,
    TerminatorKind, ValueDefinition, ValueTypeId,
};

/// Unchecked LCIR roots requested for one complete native artifact.
///
/// The variant is the artifact kind: a run artifact has exactly one root,
/// while a test artifact owns an ordered root list and may be empty. The
/// request must cross [`check_artifact`] before a backend consumes it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ArtifactRootRequest {
    Run(InstanceId),
    Tests {
        roots: Box<[InstanceId]>,
        outcomes: Box<[TestOutcomePlan]>,
    },
}

impl ArtifactRootRequest {
    /// Creates a test-root request from an owned collection or borrowed slice.
    #[must_use]
    pub fn tests(roots: impl AsRef<[InstanceId]>) -> Self {
        let roots: Box<[InstanceId]> = roots.as_ref().into();
        let outcomes = vec![TestOutcomePlan::Unit; roots.len()].into_boxed_slice();
        Self::Tests { roots, outcomes }
    }

    /// Creates an ordered test-root request with an explicit outcome plan for
    /// every root.
    #[must_use]
    pub fn planned_tests(roots: impl IntoIterator<Item = (InstanceId, TestOutcomePlan)>) -> Self {
        let (roots, outcomes): (Vec<_>, Vec<_>) = roots.into_iter().unzip();
        Self::Tests {
            roots: roots.into_boxed_slice(),
            outcomes: outcomes.into_boxed_slice(),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> ArtifactKind {
        match self {
            Self::Run(_) => ArtifactKind::Run,
            Self::Tests { .. } => ArtifactKind::Tests,
        }
    }
}

/// Checked harness interpretation for one test root.
///
/// Source-level `Result[Unit, E]` is represented as an ordinary closed sum;
/// the explicit variant plan prevents the native harness from guessing
/// semantic success from a physical tag convention.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TestOutcomePlan {
    Unit,
    Result {
        success_variant: u32,
        failure_variant: u32,
    },
}

/// Harness kind selected for a checked LCIR artifact.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ArtifactKind {
    Run,
    Tests,
}

impl fmt::Display for ArtifactKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Run => "run",
            Self::Tests => "test",
        })
    }
}

/// Stable category for an invalid LCIR artifact-root request.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ArtifactValidationCode {
    RootProgramMismatch,
    InvalidRootReference,
    DuplicateTestRoot,
    RootSignature,
    UnreachableFunction,
    ImmortalTextProvenance,
}

impl ArtifactValidationCode {
    /// Stable diagnostic code used at compiler boundaries.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RootProgramMismatch => "LcirArtifactRootProgramMismatch",
            Self::InvalidRootReference => "LcirArtifactInvalidRootReference",
            Self::DuplicateTestRoot => "LcirArtifactDuplicateTestRoot",
            Self::RootSignature => "LcirArtifactRootSignature",
            Self::UnreachableFunction => "LcirArtifactUnreachableFunction",
            Self::ImmortalTextProvenance => "LcirArtifactImmortalTextProvenance",
        }
    }
}

impl fmt::Display for ArtifactValidationCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One independently discovered artifact-root validation failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactValidationError {
    code: ArtifactValidationCode,
    path: String,
    message: String,
}

impl ArtifactValidationError {
    #[must_use]
    pub const fn code(&self) -> ArtifactValidationCode {
        self.code
    }

    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ArtifactValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} at {}: {}",
            self.code, self.path, self.message
        )
    }
}

/// All independently discoverable failures in one root request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactValidationErrors {
    errors: Vec<ArtifactValidationError>,
}

impl ArtifactValidationErrors {
    #[must_use]
    pub fn as_slice(&self) -> &[ArtifactValidationError] {
        &self.errors
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.errors.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }
}

impl fmt::Display for ArtifactValidationErrors {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "LCIR artifact-root validation failed with {} error(s)",
            self.errors.len()
        )
    }
}

impl Error for ArtifactValidationErrors {}

#[derive(Clone, Debug, Eq, PartialEq)]
enum CheckedRoots {
    Run(InstanceId),
    Tests {
        roots: Box<[InstanceId]>,
        outcomes: Box<[TestOutcomePlan]>,
    },
}

/// A complete checked LCIR program paired with independently checked roots.
///
/// This is the boundary consumed by the independent scalar LLVM object
/// emitter. It deliberately has no operation which returns an unchecked
/// [`crate::Program`] or unchecked [`ArtifactRootRequest`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedArtifact {
    program: CheckedProgram,
    roots: CheckedRoots,
}

impl CheckedArtifact {
    /// Borrows the structurally checked program without dismantling the
    /// artifact boundary.
    #[must_use]
    pub const fn program(&self) -> &CheckedProgram {
        &self.program
    }

    #[must_use]
    pub const fn kind(&self) -> ArtifactKind {
        match self.roots {
            CheckedRoots::Run(_) => ArtifactKind::Run,
            CheckedRoots::Tests { .. } => ArtifactKind::Tests,
        }
    }

    /// Returns the run root, or `None` for a test artifact.
    #[must_use]
    pub const fn run_root(&self) -> Option<InstanceId> {
        match self.roots {
            CheckedRoots::Run(root) => Some(root),
            CheckedRoots::Tests { .. } => None,
        }
    }

    /// Returns the ordered test roots, or `None` for a run artifact.
    #[must_use]
    pub fn test_roots(&self) -> Option<&[InstanceId]> {
        match &self.roots {
            CheckedRoots::Run(_) => None,
            CheckedRoots::Tests { roots, .. } => Some(roots),
        }
    }

    /// Returns the ordered explicit test outcome plans, or `None` for a run
    /// artifact.
    #[must_use]
    pub fn test_outcomes(&self) -> Option<&[TestOutcomePlan]> {
        match &self.roots {
            CheckedRoots::Run(_) => None,
            CheckedRoots::Tests { outcomes, .. } => Some(outcomes),
        }
    }

    pub(crate) fn roots(&self) -> &[InstanceId] {
        match &self.roots {
            CheckedRoots::Run(root) => std::slice::from_ref(root),
            CheckedRoots::Tests { roots, .. } => roots,
        }
    }

    /// Returns the checked representation plan needed by a target emitter.
    #[must_use]
    pub const fn representations(&self) -> &RepresentationPlan {
        self.program.as_program().representations()
    }

    /// Returns all checked function instances in deterministic table order.
    #[must_use]
    pub fn functions(&self) -> &[Function] {
        self.program.as_program().functions()
    }

    /// Resolves a branded function identity in this checked artifact.
    #[must_use]
    pub fn function(&self, id: InstanceId) -> Option<&Function> {
        self.program.as_program().function(id)
    }
}

impl CheckedProgram {
    /// Consumes a checked program and independently validates its artifact
    /// roots.
    ///
    /// # Errors
    ///
    /// Returns every independently discoverable root failure.
    pub fn into_artifact(
        self,
        roots: ArtifactRootRequest,
    ) -> Result<CheckedArtifact, ArtifactValidationErrors> {
        check_artifact(self, roots)
    }
}

/// Validates a root request against an already checked LCIR program.
///
/// The request variant makes the harness kind structurally unambiguous. This
/// validator checks branded identity, existence, per-kind signature rules,
/// uniqueness of test roots, that every function belongs to the exact callable
/// closure, and that every immortal Text value has only literal or closed
/// internal typed-flow provenance. An empty test set is valid only with an
/// empty program.
///
/// # Errors
///
/// Returns every independently discoverable root failure.
pub fn validate_artifact_roots(
    program: &CheckedProgram,
    roots: &ArtifactRootRequest,
) -> Result<(), ArtifactValidationErrors> {
    let mut validator = ArtifactValidator {
        program,
        errors: Vec::new(),
    };
    validator.validate(roots);
    if validator.errors.is_empty() {
        Ok(())
    } else {
        Err(ArtifactValidationErrors {
            errors: validator.errors,
        })
    }
}

/// Consumes a checked LCIR program and crosses the artifact-root boundary.
///
/// # Errors
///
/// Returns every independently discoverable root failure.
pub fn check_artifact(
    program: CheckedProgram,
    roots: ArtifactRootRequest,
) -> Result<CheckedArtifact, ArtifactValidationErrors> {
    validate_artifact_roots(&program, &roots)?;
    let roots = match roots {
        ArtifactRootRequest::Run(root) => CheckedRoots::Run(root),
        ArtifactRootRequest::Tests { roots, outcomes } => CheckedRoots::Tests { roots, outcomes },
    };
    Ok(CheckedArtifact { program, roots })
}

struct ArtifactValidator<'a> {
    program: &'a CheckedProgram,
    errors: Vec<ArtifactValidationError>,
}

/// A function-table-sized vector which rejects identities from another table.
///
/// Keeping the brand next to the dense storage is important: a raw index alone
/// is not an LCIR identity, and malformed roots must not be allowed to grow or
/// index the vector.
struct BrandedInstanceVec<T> {
    brand: ProgramBrand,
    entries: Vec<T>,
}

impl<T: Clone> BrandedInstanceVec<T> {
    fn filled(program: &Program, value: T) -> Self {
        Self {
            brand: program.brand,
            entries: vec![value; program.functions().len()],
        }
    }
}

impl<T> BrandedInstanceVec<T> {
    fn get(&self, instance: InstanceId) -> Option<&T> {
        if instance.brand() == self.brand {
            self.entries.get(instance.index())
        } else {
            None
        }
    }

    fn get_mut(&mut self, instance: InstanceId) -> Option<&mut T> {
        if instance.brand() == self.brand {
            self.entries.get_mut(instance.index())
        } else {
            None
        }
    }
}

impl ArtifactValidator<'_> {
    fn validate(&mut self, roots: &ArtifactRootRequest) {
        let program = self.program.as_program();
        let mut root_ids = Vec::new();
        match roots {
            ArtifactRootRequest::Run(root) => {
                self.validate_root(*root, ArtifactKind::Run, None, "roots.run".to_owned());
                root_ids.push(*root);
            }
            ArtifactRootRequest::Tests { roots, outcomes } => {
                if roots.len() != outcomes.len() {
                    self.error(
                        ArtifactValidationCode::RootSignature,
                        "roots.tests".to_owned(),
                        format!(
                            "test artifact has {} root(s), but {} outcome plan(s)",
                            roots.len(),
                            outcomes.len()
                        ),
                    );
                }
                let mut first_indices = BrandedInstanceVec::filled(program, None);
                for (index, root) in roots.iter().copied().enumerate() {
                    let path = format!("roots.tests[{index}]");
                    if let Some(first_index) = first_indices.get_mut(root) {
                        if let Some(first_index) = *first_index {
                            self.error(
                                ArtifactValidationCode::DuplicateTestRoot,
                                path.clone(),
                                format!("test root {root} duplicates roots.tests[{first_index}]"),
                            );
                        } else {
                            *first_index = Some(index);
                        }
                    }
                    self.validate_root(
                        root,
                        ArtifactKind::Tests,
                        outcomes.get(index).copied(),
                        path,
                    );
                    root_ids.push(root);
                }
            }
        }
        if self.errors.is_empty() {
            self.validate_closed_graph(&root_ids);
        }
        if self.errors.is_empty() {
            self.validate_immortal_text_provenance();
        }
    }

    fn validate_root(
        &mut self,
        root: InstanceId,
        kind: ArtifactKind,
        outcome: Option<TestOutcomePlan>,
        path: String,
    ) {
        let program = self.program.as_program();
        if root.brand() != program.brand {
            self.error(
                ArtifactValidationCode::RootProgramMismatch,
                path,
                format!("{kind} root {root} belongs to another LCIR program"),
            );
            return;
        }
        let Some(function) = program.function(root) else {
            self.error(
                ArtifactValidationCode::InvalidRootReference,
                path,
                format!("{kind} root {root} does not name an LCIR function"),
            );
            return;
        };
        let signature = function.signature();
        let valid_result = match (kind, outcome) {
            (ArtifactKind::Run, _) | (ArtifactKind::Tests, Some(TestOutcomePlan::Unit)) => program
                .representations()
                .value_type(signature.result())
                .is_some_and(|ty| ty.semantic() == &Type::Unit),
            (
                ArtifactKind::Tests,
                Some(TestOutcomePlan::Result {
                    success_variant,
                    failure_variant,
                }),
            ) => self.valid_result_outcome(signature.result(), success_variant, failure_variant),
            (ArtifactKind::Tests, None) => false,
        };
        if !signature.params().is_empty() || !valid_result {
            self.error(
                ArtifactValidationCode::RootSignature,
                path,
                format!(
                    "{kind} root {root} must declare zero parameters and match its explicit Unit or Result[Unit, E] outcome plan"
                ),
            );
        }
    }

    fn valid_result_outcome(
        &self,
        result: crate::ValueTypeId,
        success_variant: u32,
        failure_variant: u32,
    ) -> bool {
        if success_variant == failure_variant {
            return false;
        }
        let representations = self.program.as_program().representations();
        let Some(value_type) = representations.value_type(result) else {
            return false;
        };
        let Some(crate::Repr::Sum(sum)) = representations.repr(value_type.repr()).copied() else {
            return false;
        };
        let Some(sum) = representations.sum(sum) else {
            return false;
        };
        let variants = sum.variants();
        if variants.len() != 2 {
            return false;
        }
        let success = usize::try_from(success_variant)
            .ok()
            .and_then(|index| variants.get(index));
        let failure = usize::try_from(failure_variant)
            .ok()
            .and_then(|index| variants.get(index));
        let unit = representations.type_id(&Type::Unit);
        success.is_some_and(|variant| variant.fields() == unit.as_slice())
            && failure.is_some_and(|variant| variant.fields().len() == 1)
    }

    fn validate_closed_graph(&mut self, roots: &[InstanceId]) {
        let program = self.program.as_program();
        let mut reachable = BrandedInstanceVec::filled(program, false);
        let mut pending = VecDeque::new();
        for root in roots.iter().copied() {
            if let Some(is_reachable) = reachable.get_mut(root)
                && !*is_reachable
            {
                *is_reachable = true;
                pending.push_back(root);
            }
        }
        while let Some(instance) = pending.pop_front() {
            let Some(function) = program.function(instance) else {
                continue;
            };
            for instruction in function.instructions() {
                if let InstructionKind::DirectCall { callee, .. }
                | InstructionKind::TaskCreate {
                    coroutine: callee, ..
                } = instruction.kind()
                    && let Some(is_reachable) = reachable.get_mut(*callee)
                    && !*is_reachable
                {
                    *is_reachable = true;
                    pending.push_back(*callee);
                }
            }
            for block in function.blocks() {
                if let Some(terminator) = block.terminator()
                    && let TerminatorKind::Invoke { callee, .. } = terminator.kind()
                    && let Some(is_reachable) = reachable.get_mut(*callee)
                    && !*is_reachable
                {
                    *is_reachable = true;
                    pending.push_back(*callee);
                }
            }
        }
        for (index, function) in program.functions().iter().enumerate() {
            if !reachable.get(function.id()).copied().unwrap_or(false) {
                self.error(
                    ArtifactValidationCode::UnreachableFunction,
                    format!("program.function[{index}]"),
                    format!(
                        "function {} is not reachable from the checked artifact roots",
                        function.id()
                    ),
                );
            }
        }
    }

    /// Rechecks the executable half of the immortal-Text proof at the final
    /// artifact boundary. Every Text instruction result must be a literal or
    /// an internal direct-call result, and every Text block parameter must be
    /// supplied by an internal call or a checked intra-function edge. Together
    /// with zero-input roots and exact call-graph closure, this excludes an
    /// external, undefined, or moving pointer while allowing closed recursive
    /// flows that never materialize a value.
    #[expect(
        clippy::too_many_lines,
        reason = "the provenance proof exhaustively mirrors every checked LCIR edge form in one audit boundary"
    )]
    fn validate_immortal_text_provenance(&mut self) {
        let program = self.program.as_program();
        let Some(text) = program.representations().type_id(&Type::Text) else {
            return;
        };
        let is_immortal = program
            .representations()
            .value_type(text)
            .and_then(|ty| program.representations().repr(ty.repr()))
            == Some(&crate::Repr::ImmortalText);
        if !is_immortal {
            return;
        }
        let mut supplied = program
            .functions()
            .iter()
            .map(|function| vec![false; function.values().len()])
            .collect::<Vec<_>>();

        for function in program.functions() {
            for instruction in function.instructions() {
                match instruction.kind() {
                    InstructionKind::DirectCall { callee, arguments }
                    | InstructionKind::TaskCreate {
                        coroutine: callee,
                        arguments,
                    } => {
                        mark_text_call_inputs(program, text, &mut supplied, *callee, arguments);
                    }
                    _ => {}
                }
            }
            for block in function.blocks() {
                let Some(terminator) = block.terminator() else {
                    continue;
                };
                match terminator.kind() {
                    TerminatorKind::Jump(target) => {
                        mark_text_target(
                            function,
                            text,
                            &mut supplied,
                            target.block,
                            &target.arguments,
                            0,
                        );
                    }
                    TerminatorKind::Branch {
                        then_target,
                        else_target,
                        ..
                    } => {
                        mark_text_target(
                            function,
                            text,
                            &mut supplied,
                            then_target.block,
                            &then_target.arguments,
                            0,
                        );
                        mark_text_target(
                            function,
                            text,
                            &mut supplied,
                            else_target.block,
                            &else_target.arguments,
                            0,
                        );
                    }
                    TerminatorKind::SumSwitch { cases, .. }
                    | TerminatorKind::DynSwitch { cases, .. } => {
                        for case in cases {
                            let implicit = function
                                .block(case.block)
                                .map(|target| {
                                    target.params().len().saturating_sub(case.arguments.len())
                                })
                                .unwrap_or_default();
                            mark_text_target(
                                function,
                                text,
                                &mut supplied,
                                case.block,
                                &case.arguments,
                                implicit,
                            );
                        }
                    }
                    TerminatorKind::CheckedIntNegate { normal, fault, .. }
                    | TerminatorKind::CheckedIntBinary { normal, fault, .. }
                    | TerminatorKind::TaskSleep { normal, fault, .. } => {
                        mark_text_target(
                            function,
                            text,
                            &mut supplied,
                            normal.block,
                            &normal.arguments,
                            1,
                        );
                        mark_text_target(
                            function,
                            text,
                            &mut supplied,
                            fault.block,
                            &fault.arguments,
                            0,
                        );
                    }
                    TerminatorKind::Invoke {
                        callee,
                        arguments,
                        normal,
                        unwind,
                    } => {
                        mark_text_call_inputs(program, text, &mut supplied, *callee, arguments);
                        let implicit_writebacks = program
                            .function(*callee)
                            .map(|callee| callee.signature().inout_params().len())
                            .unwrap_or_default();
                        if program
                            .function(*callee)
                            .is_some_and(|callee| callee.signature().result() == text)
                            && let Some(parameter) = function
                                .block(normal.block)
                                .and_then(|block| block.params().first())
                        {
                            mark_text_value(&mut supplied, *parameter);
                        }
                        mark_text_target(
                            function,
                            text,
                            &mut supplied,
                            normal.block,
                            &normal.arguments,
                            1_usize.saturating_add(implicit_writebacks),
                        );
                        mark_text_target(
                            function,
                            text,
                            &mut supplied,
                            unwind.block,
                            &unwind.arguments,
                            implicit_writebacks,
                        );
                    }
                    TerminatorKind::AwaitTasks {
                        tasks,
                        normal,
                        fault,
                        cancel,
                        ..
                    } => {
                        for (index, task) in tasks.iter().enumerate() {
                            let output_is_text = function
                                .value(*task)
                                .and_then(|task| program.representations().value_type(task.ty()))
                                .is_some_and(|task| {
                                    matches!(task.semantic(), Type::Task(output) if output.as_ref() == &Type::Text)
                                });
                            if output_is_text
                                && let Some(parameter) = function
                                    .block(normal.block)
                                    .and_then(|block| block.params().get(index))
                            {
                                mark_text_value(&mut supplied, *parameter);
                            }
                        }
                        mark_text_target(
                            function,
                            text,
                            &mut supplied,
                            normal.block,
                            &normal.arguments,
                            tasks.len(),
                        );
                        mark_text_target(
                            function,
                            text,
                            &mut supplied,
                            fault.block,
                            &fault.arguments,
                            0,
                        );
                        mark_text_target(
                            function,
                            text,
                            &mut supplied,
                            cancel.block,
                            &cancel.arguments,
                            0,
                        );
                    }
                    TerminatorKind::ResourceClose { normal, fault, .. } => {
                        mark_text_target(
                            function,
                            text,
                            &mut supplied,
                            normal.block,
                            &normal.arguments,
                            2,
                        );
                        mark_text_target(
                            function,
                            text,
                            &mut supplied,
                            fault.block,
                            &fault.arguments,
                            1,
                        );
                    }
                    TerminatorKind::Assert { success, fault, .. } => {
                        mark_text_target(
                            function,
                            text,
                            &mut supplied,
                            success.block,
                            &success.arguments,
                            0,
                        );
                        mark_text_target(
                            function,
                            text,
                            &mut supplied,
                            fault.block,
                            &fault.arguments,
                            0,
                        );
                    }
                    TerminatorKind::Return(_)
                    | TerminatorKind::Fault { .. }
                    | TerminatorKind::ResumeFault
                    | TerminatorKind::TaskCancelled => {}
                }
            }
        }

        for (function_index, function) in program.functions().iter().enumerate() {
            for value in function.values().iter().filter(|value| value.ty() == text) {
                let valid = match value.definition() {
                    ValueDefinition::BlockParameter { .. } => supplied
                        .get(function_index)
                        .and_then(|values| values.get(value.id().index()))
                        .copied()
                        .unwrap_or(false),
                    ValueDefinition::InstructionResult { instruction, index } => function
                        .instruction(instruction)
                        .is_some_and(|instruction| match instruction.kind() {
                            InstructionKind::TextLiteral { .. } => index == 0,
                            InstructionKind::DirectCall { callee, .. } => {
                                index == 0
                                    && program
                                        .function(*callee)
                                        .is_some_and(|callee| callee.signature().result() == text)
                            }
                            _ => false,
                        }),
                };
                if !valid {
                    self.error(
                        ArtifactValidationCode::ImmortalTextProvenance,
                        format!(
                            "program.function[{function_index}].value[{}]",
                            value.id().index()
                        ),
                        "immortal Text value is not produced by a literal or supplied through closed internal typed flow".to_owned(),
                    );
                }
            }
        }
    }

    fn error(&mut self, code: ArtifactValidationCode, path: String, message: String) {
        self.errors.push(ArtifactValidationError {
            code,
            path,
            message,
        });
    }
}

fn mark_text_value(supplied: &mut [Vec<bool>], value: crate::ValueId) {
    if let Some(function) = supplied.get_mut(value.owner().index())
        && let Some(slot) = function.get_mut(value.index())
    {
        *slot = true;
    }
}

fn mark_text_call_inputs(
    program: &Program,
    text: ValueTypeId,
    supplied: &mut [Vec<bool>],
    callee: InstanceId,
    arguments: &[crate::ValueId],
) {
    let Some(callee) = program.function(callee) else {
        return;
    };
    let Some(entry) = callee.entry().and_then(|entry| callee.block(entry)) else {
        return;
    };
    for ((parameter_type, parameter), _argument) in callee
        .signature()
        .params()
        .iter()
        .copied()
        .zip(entry.params().iter().copied())
        .zip(arguments)
    {
        if parameter_type == text {
            mark_text_value(supplied, parameter);
        }
    }
}

fn mark_text_target(
    function: &Function,
    text: ValueTypeId,
    supplied: &mut [Vec<bool>],
    block: crate::BlockId,
    arguments: &[crate::ValueId],
    implicit: usize,
) {
    let Some(target) = function.block(block) else {
        return;
    };
    for (parameter, _argument) in target
        .params()
        .iter()
        .copied()
        .skip(implicit)
        .zip(arguments)
    {
        if function
            .value(parameter)
            .is_some_and(|value| value.ty() == text)
        {
            mark_text_value(supplied, parameter);
        }
    }
}

#[cfg(test)]
mod tests {
    use loom_mir::FunctionId as MirFunctionId;

    use super::*;
    use crate::{
        Constant, ContractFaultMetadata, Effects, FaultMetadata, InstructionKind, Origin,
        ProgramBuilder, ResultTarget, Signature, TargetLayout, Terminator, TerminatorKind,
        UnwindTarget, dump_program,
    };

    fn origin(function: u32) -> Origin {
        Origin::synthetic(MirFunctionId(function))
    }

    fn checked_program(
        signatures: &[(&str, Vec<Type>, Type)],
    ) -> (CheckedProgram, Vec<InstanceId>) {
        let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let mut declarations = Vec::new();
        for (index, (name, params, result)) in signatures.iter().enumerate() {
            let parameter_types = params
                .iter()
                .map(|ty| builder.type_id(ty).expect("scalar parameter type"))
                .collect::<Vec<_>>();
            let result = builder
                .type_id(result)
                .expect("scalar function result type");
            let id = builder
                .declare_function(
                    origin(u32::try_from(index).expect("test function index")),
                    *name,
                    Signature::new(parameter_types.clone(), result),
                    Effects::NONE,
                )
                .expect("declare function");
            declarations.push((id, parameter_types, result));
        }
        for (index, (id, params, result)) in declarations.iter().enumerate() {
            let mut function = builder.function(*id).expect("function builder");
            let entry = function.create_block().expect("entry");
            function.set_entry(entry).expect("set entry");
            for ty in params {
                function
                    .append_block_parameter(entry, *ty)
                    .expect("entry parameter");
            }
            let constant = match &signatures[index].2 {
                Type::Unit => Constant::Unit,
                Type::Bool => Constant::Bool(false),
                Type::Int => Constant::Int(0),
                Type::Float => Constant::float(0.0),
                _ => panic!("test helper only builds scalar results"),
            };
            let value = function
                .append_instruction(
                    entry,
                    InstructionKind::Constant(constant),
                    &[*result],
                    origin(u32::try_from(index).expect("test function index")),
                )
                .expect("result constant")[0];
            function
                .terminate(
                    entry,
                    Terminator::new(
                        TerminatorKind::Return(value),
                        origin(u32::try_from(index).expect("test function index")),
                    ),
                )
                .expect("return");
        }
        let ids = declarations.into_iter().map(|(id, _, _)| id).collect();
        (builder.finish_checked().expect("valid LCIR"), ids)
    }

    #[test]
    fn missing_same_program_root_is_rejected() {
        let (program, _) = checked_program(&[("main", Vec::new(), Type::Unit)]);
        let missing = InstanceId::from_index(program.as_program().brand, 9).expect("test id");
        let errors = validate_artifact_roots(&program, &ArtifactRootRequest::Run(missing))
            .expect_err("missing root must fail");
        assert_eq!(
            errors.as_slice(),
            &[ArtifactValidationError {
                code: ArtifactValidationCode::InvalidRootReference,
                path: "roots.run".to_owned(),
                message: "run root i9 does not name an LCIR function".to_owned(),
            }]
        );
    }

    #[test]
    fn malformed_test_roots_cannot_index_dense_validation_state() {
        let (program, _) = checked_program(&[("test", Vec::new(), Type::Unit)]);
        let huge_index = usize::try_from(u32::MAX).expect("u32 must fit usize");
        let missing = InstanceId::from_index(program.as_program().brand, huge_index)
            .expect("maximum raw identity");
        let (_foreign_program, foreign_ids) =
            checked_program(&[("foreign", Vec::new(), Type::Unit)]);

        let errors = validate_artifact_roots(
            &program,
            &ArtifactRootRequest::tests([missing, foreign_ids[0]]),
        )
        .expect_err("out-of-range and foreign roots must fail safely");

        assert_eq!(errors.len(), 2);
        assert_eq!(
            errors
                .as_slice()
                .iter()
                .map(|error| (error.code(), error.path()))
                .collect::<Vec<_>>(),
            vec![
                (
                    ArtifactValidationCode::InvalidRootReference,
                    "roots.tests[0]"
                ),
                (
                    ArtifactValidationCode::RootProgramMismatch,
                    "roots.tests[1]"
                ),
            ]
        );
    }

    #[test]
    fn root_signature_uses_semantic_unit_from_the_representation_plan() {
        let (program, ids) = checked_program(&[
            ("parameterized", vec![Type::Int], Type::Unit),
            ("returns_int", Vec::new(), Type::Int),
        ]);
        let errors = validate_artifact_roots(&program, &ArtifactRootRequest::tests(ids))
            .expect_err("invalid signatures must fail");
        assert_eq!(
            errors.as_slice(),
            &[
                ArtifactValidationError {
                    code: ArtifactValidationCode::RootSignature,
                    path: "roots.tests[0]".to_owned(),
                    message: "test root i0 must declare zero parameters and match its explicit Unit or Result[Unit, E] outcome plan".to_owned(),
                },
                ArtifactValidationError {
                    code: ArtifactValidationCode::RootSignature,
                    path: "roots.tests[1]".to_owned(),
                    message: "test root i1 must declare zero parameters and match its explicit Unit or Result[Unit, E] outcome plan".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn checked_run_artifact_preserves_kind_program_and_root_access() {
        let (program, ids) = checked_program(&[("main", Vec::new(), Type::Unit)]);
        let before = dump_program(&program);
        let artifact = program
            .into_artifact(ArtifactRootRequest::Run(ids[0]))
            .expect("valid run artifact");

        assert_eq!(artifact.kind(), ArtifactKind::Run);
        assert_eq!(artifact.run_root(), Some(ids[0]));
        assert_eq!(artifact.test_roots(), None);
        assert_eq!(artifact.function(ids[0]).map(Function::name), Some("main"));
        assert_eq!(artifact.functions().len(), 1);
        assert_eq!(
            artifact.representations().target(),
            TargetLayout::new(64).expect("target")
        );
        assert_eq!(dump_program(artifact.program()), before);
    }

    #[test]
    fn empty_test_artifact_is_valid_and_preserves_test_kind() {
        let program = ProgramBuilder::new(TargetLayout::new(64).expect("target"))
            .finish_checked()
            .expect("empty checked LCIR");
        let artifact = check_artifact(
            program,
            ArtifactRootRequest::tests(Vec::<InstanceId>::new()),
        )
        .expect("empty test suite is valid");

        assert_eq!(artifact.kind(), ArtifactKind::Tests);
        assert_eq!(artifact.run_root(), None);
        assert_eq!(artifact.test_roots(), Some([].as_slice()));
    }

    #[test]
    fn checked_artifact_rejects_functions_outside_the_root_closure() {
        let (program, ids) = checked_program(&[
            ("main", Vec::new(), Type::Unit),
            ("unreachable", Vec::new(), Type::Unit),
        ]);
        let errors = validate_artifact_roots(&program, &ArtifactRootRequest::Run(ids[0]))
            .expect_err("an artifact must contain exactly its closed callable graph");

        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors.as_slice()[0],
            ArtifactValidationError {
                code: ArtifactValidationCode::UnreachableFunction,
                path: "program.function[1]".to_owned(),
                message: "function i1 is not reachable from the checked artifact roots".to_owned(),
            }
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn checked_artifact_closes_direct_calls_and_fallible_invokes() {
        let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let unit_ty = builder.type_id(&Type::Unit).expect("Unit type");
        let leaf = builder
            .declare_function(
                origin(10),
                "leaf",
                Signature::new(Vec::new(), unit_ty),
                Effects::MAY_FAULT,
            )
            .expect("declare leaf");
        let invoking = builder
            .declare_function(
                origin(11),
                "invoking",
                Signature::new(Vec::new(), unit_ty),
                Effects::MAY_FAULT,
            )
            .expect("declare invoking function");
        let helper = builder
            .declare_function(
                origin(12),
                "helper",
                Signature::new(Vec::new(), unit_ty),
                Effects::NONE,
            )
            .expect("declare helper");

        {
            let mut function = builder.function(leaf).expect("leaf builder");
            let entry = function.create_block().expect("entry");
            function.set_entry(entry).expect("set entry");
            function
                .terminate(
                    entry,
                    Terminator::new(
                        TerminatorKind::Fault {
                            metadata: FaultMetadata::contract(ContractFaultMetadata::assertion(
                                origin(10).span,
                            )),
                        },
                        origin(10),
                    ),
                )
                .expect("fault");
        }
        {
            let mut function = builder.function(invoking).expect("invoke builder");
            let entry = function.create_block().expect("entry");
            let normal = function.create_block().expect("normal");
            let unwind = function.create_block().expect("unwind");
            function.set_entry(entry).expect("set entry");
            let result = function
                .append_block_parameter(normal, unit_ty)
                .expect("invoke result");
            function
                .terminate(
                    entry,
                    Terminator::new(
                        TerminatorKind::Invoke {
                            callee: leaf,
                            arguments: Vec::new().into_boxed_slice(),
                            normal: ResultTarget::new(normal, Vec::new()),
                            unwind: UnwindTarget::new(unwind, Vec::new()),
                        },
                        origin(11),
                    ),
                )
                .expect("invoke");
            function
                .terminate(
                    normal,
                    Terminator::new(TerminatorKind::Return(result), origin(11)),
                )
                .expect("return");
            function
                .terminate(
                    unwind,
                    Terminator::new(TerminatorKind::ResumeFault, origin(11)),
                )
                .expect("resume");
        }
        {
            let mut function = builder.function(helper).expect("helper builder");
            let entry = function.create_block().expect("entry");
            function.set_entry(entry).expect("set entry");
            let unit = function
                .append_instruction(
                    entry,
                    InstructionKind::Constant(Constant::Unit),
                    &[unit_ty],
                    origin(12),
                )
                .expect("unit")[0];
            function
                .terminate(
                    entry,
                    Terminator::new(TerminatorKind::Return(unit), origin(12)),
                )
                .expect("return");
        }

        let invoke_artifact = builder
            .finish_checked()
            .expect("valid call graph")
            .into_artifact(ArtifactRootRequest::Run(invoking))
            .expect_err("the unrelated direct-call helper remains unreachable");
        assert_eq!(
            invoke_artifact.as_slice()[0].code(),
            ArtifactValidationCode::UnreachableFunction
        );

        let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let unit_ty = builder.type_id(&Type::Unit).expect("Unit type");
        let helper = builder
            .declare_function(
                origin(13),
                "helper",
                Signature::new(Vec::new(), unit_ty),
                Effects::NONE,
            )
            .expect("declare helper");
        let root = builder
            .declare_function(
                origin(14),
                "root",
                Signature::new(Vec::new(), unit_ty),
                Effects::NONE,
            )
            .expect("declare root");
        for (id, source, call) in [(helper, 13, None), (root, 14, Some(helper))] {
            let mut function = builder.function(id).expect("function builder");
            let entry = function.create_block().expect("entry");
            function.set_entry(entry).expect("set entry");
            let value = if let Some(callee) = call {
                function
                    .append_instruction(
                        entry,
                        InstructionKind::DirectCall {
                            callee,
                            arguments: Vec::new().into_boxed_slice(),
                        },
                        &[unit_ty],
                        origin(source),
                    )
                    .expect("direct call")[0]
            } else {
                function
                    .append_instruction(
                        entry,
                        InstructionKind::Constant(Constant::Unit),
                        &[unit_ty],
                        origin(source),
                    )
                    .expect("unit")[0]
            };
            function
                .terminate(
                    entry,
                    Terminator::new(TerminatorKind::Return(value), origin(source)),
                )
                .expect("return");
        }
        let artifact = builder
            .finish_checked()
            .expect("valid direct call graph")
            .into_artifact(ArtifactRootRequest::Run(root))
            .expect("direct-call closure is complete");
        assert_eq!(artifact.functions().len(), 2);
    }

    #[test]
    fn duplicate_test_roots_report_every_duplicate_against_the_first_root() {
        let (program, ids) = checked_program(&[("test", Vec::new(), Type::Unit)]);
        let errors = validate_artifact_roots(
            &program,
            &ArtifactRootRequest::tests([ids[0], ids[0], ids[0]]),
        )
        .expect_err("duplicate tests must fail");

        assert_eq!(errors.len(), 2);
        assert_eq!(
            errors
                .as_slice()
                .iter()
                .map(ArtifactValidationError::path)
                .collect::<Vec<_>>(),
            vec!["roots.tests[1]", "roots.tests[2]"]
        );
        assert!(errors.as_slice().iter().all(|error| {
            error.code() == ArtifactValidationCode::DuplicateTestRoot
                && error.message() == "test root i0 duplicates roots.tests[0]"
        }));
    }

    #[test]
    fn equal_raw_ids_from_generative_programs_cannot_cross_artifact_boundaries() {
        let (first_program, first_ids) = checked_program(&[("first", Vec::new(), Type::Unit)]);
        let (second_program, second_ids) = checked_program(&[("second", Vec::new(), Type::Unit)]);

        assert_eq!(first_ids[0].raw(), second_ids[0].raw());
        assert_ne!(first_ids[0], second_ids[0]);
        let errors =
            validate_artifact_roots(&second_program, &ArtifactRootRequest::Run(first_ids[0]))
                .expect_err("cross-program root must fail");
        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors.as_slice()[0].code(),
            ArtifactValidationCode::RootProgramMismatch
        );
        assert_eq!(errors.as_slice()[0].path(), "roots.run");
        assert_eq!(
            errors.as_slice()[0].message(),
            "run root i0 belongs to another LCIR program"
        );

        first_program
            .into_artifact(ArtifactRootRequest::Run(first_ids[0]))
            .expect("the root remains valid in its owning program");
    }
}
