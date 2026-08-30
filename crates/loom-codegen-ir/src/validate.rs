use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::fmt;

use loom_mir::Type;

use crate::{
    AwaitMode, BlockId, Constant, Effects, Function, InstanceId, InstanceRole, Instruction,
    InstructionId, InstructionKind, IoTaskErrorMode, IoTaskOperation, Origin, ProductReprId,
    Program, Repr, RepresentationPlan, ResultTarget, SumReprId, SumTagRepr,
    TASK_OUTCOME_CANCELLED_VARIANT, TASK_OUTCOME_COMPLETED_VARIANT, TASK_OUTCOME_FAULTED_VARIANT,
    Terminator, TerminatorKind, UnwindTarget, Value, ValueDefinition, ValueId, ValueTypeId,
    ValueTypeKind,
};

fn representation_pointer_kinds(
    representations: &RepresentationPlan,
    root: ValueTypeId,
) -> Option<(bool, bool)> {
    let mut pending = vec![root];
    let mut visited = BTreeSet::new();
    let mut immortal = false;
    let mut managed = false;
    while let Some(value_id) = pending.pop() {
        if !visited.insert(value_id) {
            continue;
        }
        let value = representations.value_type(value_id)?;
        match representations.repr(value.repr())? {
            Repr::ImmortalText => immortal = true,
            Repr::ManagedPointer => managed = true,
            Repr::Product(product) => {
                pending.extend(representations.product(*product)?.fields().iter().copied());
            }
            Repr::Sum(sum) => pending.extend(
                representations
                    .sum(*sum)?
                    .variants()
                    .iter()
                    .flat_map(|variant| variant.fields().iter().copied()),
            ),
            Repr::Uninhabited | Repr::Zst | Repr::Scalar(_) | Repr::TaskHandle => {}
        }
    }
    Some((immortal, managed))
}

fn representation_contains_task_handle(
    representations: &RepresentationPlan,
    root: ValueTypeId,
) -> Option<bool> {
    let mut pending = vec![root];
    let mut visited = BTreeSet::new();
    while let Some(value_id) = pending.pop() {
        if !visited.insert(value_id) {
            continue;
        }
        let value = representations.value_type(value_id)?;
        if let ValueTypeKind::Transparent { base } = value.kind() {
            pending.push(base);
            continue;
        }
        match value.semantic() {
            Type::List(element) => pending.push(representations.type_id(element)?),
            Type::Nominal(_, arguments) if value.kind() == ValueTypeKind::ManagedTextMap => {
                let [element] = arguments.as_slice() else {
                    return None;
                };
                pending.push(representations.type_id(element)?);
            }
            Type::View { .. } => {
                pending.extend(
                    representations
                        .dynamic(value_id)?
                        .candidates()
                        .iter()
                        .copied(),
                );
            }
            _ => {}
        }
        match representations.repr(value.repr())? {
            Repr::TaskHandle => return Some(true),
            Repr::Product(product) => {
                pending.extend(representations.product(*product)?.fields().iter().copied());
            }
            Repr::Sum(sum) => pending.extend(
                representations
                    .sum(*sum)?
                    .variants()
                    .iter()
                    .flat_map(|variant| variant.fields().iter().copied()),
            ),
            Repr::Uninhabited
            | Repr::Zst
            | Repr::Scalar(_)
            | Repr::ImmortalText
            | Repr::ManagedPointer => {}
        }
    }
    Some(false)
}

fn semantic_type_is_task_free(root: &Type) -> bool {
    let mut pending = vec![root];
    let mut visited = 0_usize;
    while let Some(ty) = pending.pop() {
        visited = visited.saturating_add(1);
        if visited > crate::repr::DIRECT_PRODUCT_MAX_STRUCTURAL_NODES {
            return false;
        }
        match ty {
            Type::Tuple(elements) | Type::Nominal(_, elements) => pending.extend(elements),
            Type::List(element) | Type::TaskOutcome(element) => pending.push(element),
            Type::View { bindings, .. } => pending.extend(bindings.values()),
            Type::Task(_)
            | Type::Parameter(_)
            | Type::AssociatedProjection { .. }
            | Type::Error => return false,
            Type::Never | Type::Unit | Type::Bool | Type::Int | Type::Float | Type::Text => {}
        }
    }
    true
}

fn managed_list_element_is_valid(representations: &RepresentationPlan, semantic: &Type) -> bool {
    let Some(element_id) = representations.type_id(semantic) else {
        return false;
    };
    let Some(element) = representations.value_type(element_id) else {
        return false;
    };
    let Some(repr) = representations.repr(element.repr()) else {
        return false;
    };
    element.semantic() != &Type::Never
        && matches!(
            repr,
            Repr::Zst
                | Repr::Scalar(_)
                | Repr::ManagedPointer
                | Repr::TaskHandle
                | Repr::Product(_)
                | Repr::Sum(_)
        )
        && ((repr == &Repr::TaskHandle
            && element.kind() == ValueTypeKind::Direct
            && matches!(element.semantic(), Type::Task(_)))
            || (semantic_type_is_task_free(element.semantic())
                && representation_contains_task_handle(representations, element_id) == Some(false)))
        && representation_pointer_kinds(representations, element_id)
            .is_some_and(|(immortal, _)| !immortal)
}

fn managed_text_map_value_is_valid(representations: &RepresentationPlan, semantic: &Type) -> bool {
    let Some(value_id) = representations.type_id(semantic) else {
        return false;
    };
    let Some(value) = representations.value_type(value_id) else {
        return false;
    };
    value.semantic() != &Type::Never
        && semantic_type_is_task_free(value.semantic())
        && representation_contains_task_handle(representations, value_id) == Some(false)
        && matches!(
            representations.repr(value.repr()),
            Some(
                Repr::Zst
                    | Repr::Scalar(_)
                    | Repr::ManagedPointer
                    | Repr::Product(_)
                    | Repr::Sum(_)
            )
        )
        && representation_pointer_kinds(representations, value_id)
            .is_some_and(|(immortal, _)| !immortal)
}

fn managed_pointer_semantic_is_valid(program: &Program, ty: ValueTypeId) -> bool {
    let representations = program.representations();
    let Some(value_type) = representations.value_type(ty) else {
        return false;
    };
    match (value_type.kind(), value_type.semantic()) {
        (ValueTypeKind::Transparent { base }, Type::Nominal(_, _)) => representations
            .value_type(base)
            .is_some_and(|base| representations.repr(base.repr()) == Some(&Repr::ManagedPointer)),
        (ValueTypeKind::Direct, Type::Text) => true,
        (ValueTypeKind::Direct, Type::Nominal(identity, arguments))
            if Some(*identity) == program.canonical_types().bytes && arguments.is_empty() =>
        {
            true
        }
        (ValueTypeKind::Direct, Type::List(element)) => {
            managed_list_element_is_valid(representations, element)
        }
        (ValueTypeKind::ManagedTextMap, Type::Nominal(identity, arguments))
            if Some(*identity) == program.canonical_types().text_map =>
        {
            let [value] = arguments.as_slice() else {
                return false;
            };
            managed_text_map_value_is_valid(representations, value)
        }
        (ValueTypeKind::Direct, Type::View { .. }) => representations.dynamic(ty).is_some(),
        _ => false,
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ValidationCode {
    CanonicalTypeCatalog,
    RepresentationPlan,
    InstancePlan,
    InstanceKeyStructureBudget,
    OpenInstanceKey,
    IndexMismatch,
    InvalidFunctionReference,
    InvalidBlockReference,
    InvalidInstructionReference,
    InvalidValueReference,
    InvalidTypeReference,
    MissingEntry,
    EntrySignature,
    EntryPredecessor,
    MissingTerminator,
    InstructionSchedule,
    ValueDefinition,
    InstructionShape,
    TypeMismatch,
    BlockArgument,
    ReturnType,
    CallShape,
    InOutShape,
    EffectImplication,
    EffectMismatch,
    FaultMetadata,
    FaultState,
    OriginMismatch,
    DuplicateSuccessor,
    UninhabitedValue,
    UnreachableBlock,
    Dominance,
    InvalidIntegerProof,
    InvalidListUniqueness,
    InvalidTaskOwnership,
    InvalidCoroutinePlan,
}

impl ValidationCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CanonicalTypeCatalog => "LcirCanonicalTypeCatalog",
            Self::RepresentationPlan => "LcirRepresentationPlan",
            Self::InstancePlan => "LcirInstancePlan",
            Self::InstanceKeyStructureBudget => "LcirInstanceKeyStructureBudget",
            Self::OpenInstanceKey => "LcirOpenInstanceKey",
            Self::IndexMismatch => "LcirIndexMismatch",
            Self::InvalidFunctionReference => "LcirInvalidFunctionReference",
            Self::InvalidBlockReference => "LcirInvalidBlockReference",
            Self::InvalidInstructionReference => "LcirInvalidInstructionReference",
            Self::InvalidValueReference => "LcirInvalidValueReference",
            Self::InvalidTypeReference => "LcirInvalidTypeReference",
            Self::MissingEntry => "LcirMissingEntry",
            Self::EntrySignature => "LcirEntrySignature",
            Self::EntryPredecessor => "LcirEntryPredecessor",
            Self::MissingTerminator => "LcirMissingTerminator",
            Self::InstructionSchedule => "LcirInstructionSchedule",
            Self::ValueDefinition => "LcirValueDefinition",
            Self::InstructionShape => "LcirInstructionShape",
            Self::TypeMismatch => "LcirTypeMismatch",
            Self::BlockArgument => "LcirBlockArgument",
            Self::ReturnType => "LcirReturnType",
            Self::CallShape => "LcirCallShape",
            Self::InOutShape => "LcirInOutShape",
            Self::EffectImplication => "LcirEffectImplication",
            Self::EffectMismatch => "LcirEffectMismatch",
            Self::FaultMetadata => "LcirFaultMetadata",
            Self::FaultState => "LcirFaultState",
            Self::OriginMismatch => "LcirOriginMismatch",
            Self::DuplicateSuccessor => "LcirDuplicateSuccessor",
            Self::UninhabitedValue => "LcirUninhabitedValue",
            Self::UnreachableBlock => "LcirUnreachableBlock",
            Self::Dominance => "LcirDominance",
            Self::InvalidIntegerProof => "LcirInvalidIntegerProof",
            Self::InvalidListUniqueness => "LcirInvalidListUniqueness",
            Self::InvalidTaskOwnership => "LcirInvalidTaskOwnership",
            Self::InvalidCoroutinePlan => "LcirInvalidCoroutinePlan",
        }
    }
}

impl fmt::Display for ValidationCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationError {
    code: ValidationCode,
    path: String,
    message: String,
}

impl ValidationError {
    #[must_use]
    pub const fn code(&self) -> ValidationCode {
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

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} at {}: {}",
            self.code, self.path, self.message
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationErrors {
    errors: Vec<ValidationError>,
}

impl ValidationErrors {
    #[must_use]
    pub fn as_slice(&self) -> &[ValidationError] {
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

impl fmt::Display for ValidationErrors {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "LCIR validation failed with {} error(s)",
            self.errors.len()
        )
    }
}

impl Error for ValidationErrors {}

/// An owned LCIR program which crossed the complete structural validator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedProgram {
    program: Program,
}

impl CheckedProgram {
    #[must_use]
    pub const fn as_program(&self) -> &Program {
        &self.program
    }

    /// Removes the checked wrapper. The returned value no longer carries the
    /// type-level guarantee required by code-generation consumers.
    #[must_use]
    pub fn into_unchecked(self) -> Program {
        self.program
    }
}

impl Program {
    /// Validates this LCIR program without consuming it.
    ///
    /// # Errors
    ///
    /// Returns all independently discoverable structural failures.
    pub fn validate(&self) -> Result<(), ValidationErrors> {
        validate_program(self)
    }

    /// Consumes and validates this LCIR program.
    ///
    /// # Errors
    ///
    /// Returns all independently discoverable structural failures.
    pub fn into_checked(self) -> Result<CheckedProgram, ValidationErrors> {
        check_program(self)
    }
}

/// Validates a borrowed LCIR program.
///
/// # Errors
///
/// Returns all independently discoverable structural failures.
pub fn validate_program(program: &Program) -> Result<(), ValidationErrors> {
    let errors = Validator::new(program).run();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors { errors })
    }
}

/// Validates and wraps an owned LCIR program.
///
/// # Errors
///
/// Returns all independently discoverable structural failures.
pub fn check_program(program: Program) -> Result<CheckedProgram, ValidationErrors> {
    validate_program(&program)?;
    Ok(CheckedProgram { program })
}

struct Validator<'a> {
    program: &'a Program,
    fault_states: Vec<Vec<FaultStateSet>>,
    exact_effects: Vec<Effects>,
    text_literal_bytes: usize,
    errors: Vec<ValidationError>,
}

impl<'a> Validator<'a> {
    fn new(program: &'a Program) -> Self {
        let fault_states = compute_program_fault_states(program);
        let exact_effects = compute_exact_effects(program, &fault_states);
        Self {
            program,
            fault_states,
            exact_effects,
            text_literal_bytes: 0,
            errors: Vec::new(),
        }
    }

    fn run(mut self) -> Vec<ValidationError> {
        self.validate_canonical_types();
        self.validate_instances();
        self.validate_representations();
        for (index, function) in self.program.functions.iter().enumerate() {
            let expected = InstanceId::from_index(self.program.brand, index);
            if expected != Some(function.id) {
                self.error(
                    ValidationCode::IndexMismatch,
                    format!("function[{index}]"),
                    format!(
                        "function table index {index} carries identity {}",
                        function.id
                    ),
                );
            }
            if let Some(key) = self
                .program
                .instances
                .entries
                .get(index)
                .map(|entry| &entry.key)
                && key.source() != function.origin.source_function
            {
                self.error(
                    ValidationCode::OriginMismatch,
                    format!("function[{index}].origin.source"),
                    format!(
                        "function origin source #{} does not match instance-key source #{}",
                        function.origin.source_function.0,
                        key.source().0
                    ),
                );
            }
            self.validate_function(function, index);
        }
        self.errors
    }

    fn validate_canonical_types(&mut self) {
        let canonical = *self.program.canonical_types();
        let mut identities = BTreeMap::new();
        for (name, identity) in [
            ("result", canonical.result),
            ("option", canonical.option),
            ("constraint_error", canonical.constraint_error),
            ("task_fault", canonical.task_fault),
            ("task_outcome", canonical.task_outcome),
            ("duration", canonical.duration),
            ("file", canonical.file),
            ("socket", canonical.socket),
            ("bytes", canonical.bytes),
            ("path", canonical.path),
            ("decode_text_error", canonical.decode_text_error),
            ("path_error", canonical.path_error),
            ("text_map", canonical.text_map),
            ("json", canonical.json),
            ("json_error", canonical.json_error),
            ("io_error", canonical.io_error),
            ("io_error_kind", canonical.io_error_kind),
            ("log_level", canonical.log_level),
        ] {
            let Some(identity) = identity else {
                continue;
            };
            if let Some(previous) = identities.insert(identity, name) {
                self.error(
                    ValidationCode::CanonicalTypeCatalog,
                    format!("canonical_types.{name}"),
                    format!(
                        "canonical type identity #{} is already assigned to {previous}",
                        identity.0
                    ),
                );
            }
        }
    }

    fn validate_instances(&mut self) {
        if self.program.instances.brand != self.program.brand {
            self.error(
                ValidationCode::InstancePlan,
                "instances",
                "instance plan belongs to a different LCIR program",
            );
        }
        if self.program.instances.entries.len() != self.program.functions.len() {
            self.error(
                ValidationCode::InstancePlan,
                "instances",
                format!(
                    "instance plan has {} entries, but the function table has {}",
                    self.program.instances.entries.len(),
                    self.program.functions.len()
                ),
            );
        }

        let mut identities = BTreeMap::new();
        for (index, instance) in self.program.instances.entries.iter().enumerate() {
            let expected = InstanceId::from_index(self.program.brand, index);
            if expected != Some(instance.id) {
                self.error(
                    ValidationCode::IndexMismatch,
                    format!("instances[{index}]"),
                    format!(
                        "instance-plan table index {index} carries identity {}",
                        instance.id
                    ),
                );
            }
            if let Err(error) = instance.key.validate_structure() {
                let (code, message) = match error {
                    crate::instance::InstanceKeyStructureError::BudgetExceeded => (
                        ValidationCode::InstanceKeyStructureBudget,
                        format!(
                            "instance key exceeds the {}-node structural budget",
                            crate::INSTANCE_KEY_STRUCTURE_BUDGET
                        ),
                    ),
                    crate::instance::InstanceKeyStructureError::OpenArgument => (
                        ValidationCode::OpenInstanceKey,
                        "instance key contains an unresolved type or witness parameter".to_owned(),
                    ),
                };
                self.error(code, format!("instances[{index}].key"), message);
                continue;
            }
            let identity = instance.key.canonical_identity();
            if let Some(previous) = identities.insert(identity, index) {
                self.error(
                    ValidationCode::InstancePlan,
                    format!("instances[{index}].key"),
                    format!("instance key duplicates instances[{previous}].key"),
                );
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn validate_representations(&mut self) {
        let expected = RepresentationPlan::direct_with_brand(
            self.program.representations.target(),
            self.program.brand,
        );
        let representations = self.program.representations.clone();
        let direct_reprs = expected.reprs().len();
        let direct_types = expected.value_types().len();
        let direct_registrations = expected.registrations().len();
        if representations.reprs().get(..direct_reprs) != Some(expected.reprs())
            || representations.value_types().get(..direct_types) != Some(expected.value_types())
            || representations.registrations().get(..direct_registrations)
                != Some(expected.registrations())
        {
            self.error(
                ValidationCode::RepresentationPlan,
                "representations",
                "LCIR representation table does not begin with the canonical direct catalog",
            );
        }
        let mut rebuilt_canonical_types = BTreeMap::new();
        for (index, registration) in representations.registrations().iter().enumerate() {
            if rebuilt_canonical_types
                .insert(registration.semantic().clone(), registration.value_type())
                .is_some()
            {
                self.error(
                    ValidationCode::RepresentationPlan,
                    format!("representations.registration[{index}]"),
                    format!(
                        "canonical semantic registration {:?} appears more than once",
                        registration.semantic()
                    ),
                );
            }
            match representations.value_type(registration.value_type()) {
                Some(value_type) if value_type.semantic() == registration.semantic() => {}
                Some(value_type) => self.error(
                    ValidationCode::RepresentationPlan,
                    format!("representations.registration[{index}]"),
                    format!(
                        "registration semantic {:?} does not match value type {} semantic {:?}",
                        registration.semantic(),
                        registration.value_type(),
                        value_type.semantic()
                    ),
                ),
                None => self.error(
                    ValidationCode::InvalidTypeReference,
                    format!("representations.registration[{index}]"),
                    format!(
                        "registration references missing value type {}",
                        registration.value_type()
                    ),
                ),
            }
        }
        if &rebuilt_canonical_types != representations.canonical_types() {
            self.error(
                ValidationCode::RepresentationPlan,
                "representations.canonical_types",
                "canonical type index does not exactly match the ordered registration table",
            );
        }

        let product_count = representations.products().len();
        let sum_count = representations.sums().len();
        let mut product_repr_uses = vec![0_usize; product_count];
        let mut sum_repr_uses = vec![0_usize; sum_count];
        for (index, repr) in representations.reprs().iter().copied().enumerate() {
            match repr {
                Repr::Product(product) => {
                    if let Some(uses) = product_repr_uses.get_mut(product.index())
                        && representations.product(product).is_some()
                    {
                        *uses = uses.saturating_add(1);
                    } else {
                        self.error(
                            ValidationCode::RepresentationPlan,
                            format!("representations.repr[{index}]"),
                            format!("product representation references missing product {product}"),
                        );
                    }
                }
                Repr::Sum(sum) => {
                    if let Some(uses) = sum_repr_uses.get_mut(sum.index())
                        && representations.sum(sum).is_some()
                    {
                        *uses = uses.saturating_add(1);
                    } else {
                        self.error(
                            ValidationCode::RepresentationPlan,
                            format!("representations.repr[{index}]"),
                            format!("sum representation references missing sum {sum}"),
                        );
                    }
                }
                Repr::Uninhabited
                | Repr::Zst
                | Repr::Scalar(_)
                | Repr::ImmortalText
                | Repr::ManagedPointer
                | Repr::TaskHandle => {}
            }
        }
        let mut product_value_uses = vec![0_usize; product_count];
        let mut sum_value_uses = vec![0_usize; sum_count];
        let canonical_bytes_semantic = self
            .program
            .canonical_types
            .bytes
            .map(|bytes| Type::Nominal(bytes, Vec::new()));
        for (index, value_type) in representations.value_types().iter().enumerate() {
            match value_type.kind() {
                ValueTypeKind::Direct => {}
                ValueTypeKind::ManagedTextMap => {
                    let valid = matches!(value_type.semantic(), Type::Nominal(identity, arguments)
                        if Some(*identity) == self.program.canonical_types.text_map
                            && arguments.len() == 1)
                        && matches!(
                            representations.repr(value_type.repr()),
                            Some(Repr::ManagedPointer)
                        )
                        && representations.type_id(&Type::Text).is_some_and(|text| {
                            representations.value_type(text).is_some_and(|text| {
                                text.semantic() == &Type::Text
                                    && text.kind() == ValueTypeKind::Direct
                                    && matches!(
                                        representations.repr(text.repr()),
                                        Some(Repr::ManagedPointer)
                                    )
                            })
                        });
                    if !valid {
                        self.error(
                            ValidationCode::RepresentationPlan,
                            format!("representations.type[{index}].managed_text_map"),
                            "managed TextMap must be a unary nominal value backed by one managed pointer with canonical managed Text keys",
                        );
                    }
                }
                ValueTypeKind::Transparent { base } => {
                    let valid = representations.value_type(base).is_some_and(|base_type| {
                        base_type.semantic() != &Type::Never
                            && representations
                                .repr(base_type.repr())
                                .is_some_and(|repr| !matches!(repr, Repr::Uninhabited))
                            && base.index() < index
                            && base_type.semantic() != value_type.semantic()
                            && base_type.repr() == value_type.repr()
                            && crate::repr::is_concrete_nominal_type(value_type.semantic())
                    });
                    if !valid {
                        self.error(
                            ValidationCode::RepresentationPlan,
                            format!("representations.type[{index}].transparent"),
                            "transparent nominal type must name an inhabited base value type with the exact same representation",
                        );
                    }
                }
                ValueTypeKind::InvariantProduct => {
                    if !crate::repr::is_concrete_nominal_type(value_type.semantic())
                        || !matches!(
                            representations.repr(value_type.repr()),
                            Some(Repr::Product(_))
                        )
                    {
                        self.error(
                            ValidationCode::RepresentationPlan,
                            format!("representations.type[{index}].invariant_product"),
                            "invariant-product type must be a concrete nominal value backed by a product representation",
                        );
                    }
                }
            }
            let canonical_protection = representations
                .type_id(value_type.semantic())
                .and_then(|canonical| representations.value_type(canonical))
                .is_some_and(|canonical| match (canonical.kind(), value_type.kind()) {
                    (ValueTypeKind::Direct, ValueTypeKind::Direct)
                    | (ValueTypeKind::ManagedTextMap, ValueTypeKind::ManagedTextMap)
                    | (ValueTypeKind::InvariantProduct, ValueTypeKind::InvariantProduct) => true,
                    (
                        ValueTypeKind::Transparent {
                            base: canonical_base,
                        },
                        ValueTypeKind::Transparent { base },
                    ) => representations
                        .value_type(canonical_base)
                        .zip(representations.value_type(base))
                        .is_some_and(|(canonical_base, base)| {
                            canonical_base.semantic() == base.semantic()
                        }),
                    (
                        ValueTypeKind::Direct
                        | ValueTypeKind::ManagedTextMap
                        | ValueTypeKind::InvariantProduct
                        | ValueTypeKind::Transparent { .. },
                        _,
                    ) => false,
                });
            if !canonical_protection {
                self.error(
                    ValidationCode::RepresentationPlan,
                    format!("representations.type[{index}].kind"),
                    "every representation alternative for a semantic type must inherit its canonical construction protection and transparent base relation",
                );
            }
            if let ValueTypeKind::Transparent { base } = value_type.kind() {
                if representation_pointer_kinds(&representations, base)
                    .is_none_or(|(immortal, _)| immortal)
                {
                    self.error(
                        ValidationCode::RepresentationPlan,
                        format!("representations.type[{index}].kind.transparent_base"),
                        "transparent values cannot retain an immortal-only Text base representation",
                    );
                }
                if representations.is_exact_task_list(base) {
                    self.error(
                        ValidationCode::RepresentationPlan,
                        format!("representations.type[{index}].kind.transparent_base"),
                        "exact List[Task[T]] cannot be hidden behind a transparent carrier",
                    );
                }
            }
            if canonical_bytes_semantic.as_ref() == Some(value_type.semantic())
                && (value_type.kind() != ValueTypeKind::Direct
                    || representations.repr(value_type.repr()) != Some(&Repr::ManagedPointer)
                    || representations.target().pointer_bits() != 64)
            {
                self.error(
                    ValidationCode::RepresentationPlan,
                    format!("representations.type[{index}].bytes_pointer"),
                    "canonical Bytes must be one direct managed pointer on a 64-bit target",
                );
            }
            match representations.repr(value_type.repr()).copied() {
                Some(Repr::Product(product)) => {
                    match value_type.semantic() {
                        semantic if crate::repr::is_concrete_nominal_type(semantic) => {}
                        Type::Tuple(elements) => {
                            if let Some(fields) = representations
                                .product(product)
                                .map(crate::ProductRepr::fields)
                            {
                                if fields.len() != elements.len() {
                                    self.error(
                                        ValidationCode::RepresentationPlan,
                                        format!("representations.type[{index}]"),
                                        "tuple semantic arity does not match its product representation",
                                    );
                                }
                                for (field_index, (field, element)) in
                                    fields.iter().zip(elements).enumerate()
                                {
                                    if representations
                                        .value_type(*field)
                                        .is_none_or(|field_type| field_type.semantic() != element)
                                    {
                                        self.error(
                                            ValidationCode::RepresentationPlan,
                                            format!(
                                                "representations.type[{index}].field[{field_index}]"
                                            ),
                                            "tuple semantic element does not match its product field type",
                                        );
                                    }
                                }
                            }
                        }
                        _ => {
                            self.error(
                                ValidationCode::RepresentationPlan,
                                format!("representations.type[{index}]"),
                                "direct product value types require a structural tuple or concrete nominal semantic type",
                            );
                        }
                    }
                    if let Some(uses) = product_value_uses.get_mut(product.index()) {
                        *uses = uses.saturating_add(1);
                    }
                }
                Some(Repr::Sum(sum)) => {
                    if !matches!(value_type.semantic(), Type::Nominal(_, _)) {
                        self.error(
                            ValidationCode::RepresentationPlan,
                            format!("representations.type[{index}]"),
                            "direct sum value types require a concrete nominal semantic type",
                        );
                    }
                    if let Some(uses) = sum_value_uses.get_mut(sum.index()) {
                        *uses = uses.saturating_add(1);
                    }
                }
                Some(Repr::ImmortalText) => {
                    if value_type.semantic() != &Type::Text
                        || value_type.kind() != ValueTypeKind::Direct
                        || representations.target().pointer_bits() != 64
                    {
                        self.error(
                            ValidationCode::RepresentationPlan,
                            format!("representations.type[{index}].text_pointer"),
                            "immortal Text pointers must be the direct Text semantic type on a 64-bit target",
                        );
                    }
                }
                Some(Repr::ManagedPointer) => {
                    let ty = ValueTypeId::from_index(self.program.brand, index)
                        .unwrap_or_else(|| unreachable!("validated table index fits"));
                    if !managed_pointer_semantic_is_valid(self.program, ty)
                        || representations.target().pointer_bits() != 64
                    {
                        self.error(
                            ValidationCode::RepresentationPlan,
                            format!("representations.type[{index}].managed_pointer"),
                            "managed pointers must be direct Text, canonical Bytes, concrete closed List, compiler-private closed TextMap, cataloged closed dynamic View, or a transparent alias of one on a 64-bit target",
                        );
                    }
                }
                Some(Repr::TaskHandle) => {
                    let canonical =
                        ValueTypeId::from_index(self.program.brand, index).is_some_and(|ty| {
                            representations.type_id(value_type.semantic()) == Some(ty)
                        });
                    let valid_semantic = match (value_type.kind(), value_type.semantic()) {
                        (ValueTypeKind::Direct, Type::Task(output)) => {
                            representations.type_id(output).is_some_and(|output_id| {
                                representations.value_type(output_id).is_some_and(|output| {
                                    output.semantic() != &Type::Never
                                        && semantic_type_is_task_free(output.semantic())
                                        && representation_contains_task_handle(
                                            &representations,
                                            output_id,
                                        ) == Some(false)
                                })
                            })
                        }
                        (ValueTypeKind::Transparent { base }, Type::Nominal(_, _)) => {
                            representations.value_type(base).is_some_and(|base| {
                                representations.repr(base.repr()) == Some(&Repr::TaskHandle)
                            })
                        }
                        _ => false,
                    };
                    if !canonical
                        || !valid_semantic
                        || representations.target().pointer_bits() != 64
                    {
                        self.error(
                            ValidationCode::RepresentationPlan,
                            format!("representations.type[{index}].task_handle"),
                            "Task handles must be direct Task[output] values with a registered inhabited task-free output, or transparent constrained wrappers over one, on a 64-bit target",
                        );
                    }
                }
                Some(Repr::Uninhabited | Repr::Zst | Repr::Scalar(_)) | None => {
                    if value_type.semantic() == &Type::Text
                        || canonical_bytes_semantic.as_ref() == Some(value_type.semantic())
                        || matches!(value_type.semantic(), Type::List(_))
                        || value_type.kind() == ValueTypeKind::ManagedTextMap
                        || matches!(value_type.semantic(), Type::Task(_))
                    {
                        self.error(
                            ValidationCode::RepresentationPlan,
                            format!("representations.type[{index}]"),
                            "Text, Bytes, List, Task, and managed TextMap values must use their canonical pointer representations",
                        );
                    }
                }
            }
        }

        let mut dynamic_views = BTreeSet::new();
        for (index, dynamic) in representations.dynamics().iter().enumerate() {
            let valid_view = representations
                .value_type(dynamic.view())
                .is_some_and(|view| {
                    matches!(view.semantic(), Type::View { .. })
                        && view.kind() == ValueTypeKind::Direct
                        && matches!(
                            representations.repr(view.repr()),
                            Some(Repr::ManagedPointer)
                        )
                        && representations.type_id(view.semantic()) == Some(dynamic.view())
                });
            if !valid_view || !dynamic_views.insert(dynamic.view()) {
                self.error(
                    ValidationCode::RepresentationPlan,
                    format!("representations.dynamic[{index}].view"),
                    "dynamic catalog must name one unique canonical direct managed View",
                );
            }
            if dynamic.candidates().len() < 2 {
                self.error(
                    ValidationCode::RepresentationPlan,
                    format!("representations.dynamic[{index}].candidates"),
                    "dynamic catalog requires at least two concrete candidates",
                );
            }
            let mut candidates = BTreeSet::new();
            for (candidate_index, candidate) in dynamic.candidates().iter().copied().enumerate() {
                let valid = candidates.insert(candidate)
                    && representations
                        .value_type(candidate)
                        .is_some_and(|candidate_type| {
                            candidate_type.semantic() != &Type::Never
                                && !matches!(
                                    representations.repr(candidate_type.repr()),
                                    Some(Repr::Uninhabited)
                                )
                                && representation_contains_task_handle(&representations, candidate)
                                    == Some(false)
                                && semantic_type_is_task_free(candidate_type.semantic())
                                && representation_pointer_kinds(&representations, candidate)
                                    .is_some_and(|(immortal, _)| !immortal)
                        });
                if !valid {
                    self.error(
                        ValidationCode::RepresentationPlan,
                        format!("representations.dynamic[{index}].candidate[{candidate_index}]"),
                        "dynamic candidate must be distinct, inhabited, registered, and contain neither Task handles nor immortal-only pointer leaves",
                    );
                }
            }
        }

        let aggregate_count = product_count.saturating_add(sum_count);
        let mut aggregate_edges = vec![Vec::new(); aggregate_count];
        let mut aggregate_costs = vec![0_usize; aggregate_count];
        let aggregate_index = |field: ValueTypeId| {
            representations
                .value_type(field)
                .and_then(|value_type| representations.repr(value_type.repr()))
                .and_then(|repr| match repr {
                    Repr::Product(product) if representations.product(*product).is_some() => {
                        Some(product.index())
                    }
                    Repr::Sum(sum) if representations.sum(*sum).is_some() => {
                        product_count.checked_add(sum.index())
                    }
                    Repr::Uninhabited
                    | Repr::Zst
                    | Repr::Scalar(_)
                    | Repr::ImmortalText
                    | Repr::ManagedPointer
                    | Repr::TaskHandle
                    | Repr::Product(_)
                    | Repr::Sum(_) => None,
                })
        };
        let supported_product_field = |field: ValueTypeId| {
            representations.value_type(field).is_some_and(|value_type| {
                value_type.semantic() != &Type::Never
                    && !representations.is_exact_task_list(field)
                    && matches!(
                        representations.repr(value_type.repr()),
                        Some(
                            Repr::Zst
                                | Repr::Scalar(_)
                                | Repr::ManagedPointer
                                | Repr::TaskHandle
                                | Repr::Product(_)
                                | Repr::Sum(_)
                        )
                    )
            })
        };
        let supported_sum_field = |field: ValueTypeId| {
            supported_product_field(field)
                && representation_pointer_kinds(&representations, field)
                    .is_some_and(|(immortal, _)| !immortal)
        };
        for (index, product) in representations.products().iter().enumerate() {
            let product_id = ProductReprId::from_index(self.program.brand, index);
            if product_id.is_none() || product_repr_uses.get(index) != Some(&1) {
                self.error(
                    ValidationCode::RepresentationPlan,
                    format!("representations.product[{index}].repr"),
                    "each product definition must have exactly one representation-table entry",
                );
            }
            if product_value_uses.get(index).copied().unwrap_or_default() == 0 {
                self.error(
                    ValidationCode::RepresentationPlan,
                    format!("representations.product[{index}].type"),
                    "each product definition must be used by at least one value type",
                );
            }
            aggregate_costs[index] = 1_usize.saturating_add(product.fields().len());
            for (field_index, field) in product.fields().iter().copied().enumerate() {
                if representations.is_exact_task_list(field) {
                    self.error(
                        ValidationCode::RepresentationPlan,
                        format!("representations.product[{index}].field[{field_index}]"),
                        "exact List[Task[T]] is a top-level affine carrier and cannot be nested in a product",
                    );
                } else if !supported_product_field(field) {
                    self.error(
                        ValidationCode::RepresentationPlan,
                        format!("representations.product[{index}].field[{field_index}]"),
                        "product fields must reference inhabited direct values; Text leaves require ManagedPointer",
                    );
                }
                if let Some(nested) = aggregate_index(field) {
                    aggregate_edges[index].push(nested);
                }
            }
        }
        for (index, sum) in representations.sums().iter().enumerate() {
            let aggregate = product_count.saturating_add(index);
            let sum_id = SumReprId::from_index(self.program.brand, index);
            if sum_id.is_none() || sum_repr_uses.get(index) != Some(&1) {
                self.error(
                    ValidationCode::RepresentationPlan,
                    format!("representations.sum[{index}].repr"),
                    "each sum definition must have exactly one representation-table entry",
                );
            }
            if sum_value_uses.get(index).copied().unwrap_or_default() == 0 {
                self.error(
                    ValidationCode::RepresentationPlan,
                    format!("representations.sum[{index}].type"),
                    "each sum definition must be used by at least one value type",
                );
            }
            if sum.variants().is_empty() {
                self.error(
                    ValidationCode::RepresentationPlan,
                    format!("representations.sum[{index}].variants"),
                    "direct sums must have at least one closed variant",
                );
            }
            if SumTagRepr::for_variant_count(sum.variants().len()) != Some(sum.tag()) {
                self.error(
                    ValidationCode::RepresentationPlan,
                    format!("representations.sum[{index}].tag"),
                    "sum tag representation is not canonical for its variant count",
                );
            }
            let payload_fields = sum
                .variants()
                .iter()
                .map(|variant| variant.fields().len())
                .sum::<usize>();
            aggregate_costs[aggregate] = 1_usize
                .saturating_add(sum.variants().len())
                .saturating_add(payload_fields);
            for (variant_index, variant) in sum.variants().iter().enumerate() {
                for (field_index, field) in variant.fields().iter().copied().enumerate() {
                    if representations.is_exact_task_list(field) {
                        self.error(
                            ValidationCode::RepresentationPlan,
                            format!(
                                "representations.sum[{index}].variant[{variant_index}].field[{field_index}]"
                            ),
                            "exact List[Task[T]] is a top-level affine carrier and cannot be nested in a sum",
                        );
                    } else if !supported_sum_field(field) {
                        self.error(
                            ValidationCode::RepresentationPlan,
                            format!(
                                "representations.sum[{index}].variant[{variant_index}].field[{field_index}]"
                            ),
                            "sum payloads must reference inhabited direct values; Text leaves require ManagedPointer",
                        );
                    }
                    if let Some(nested) = aggregate_index(field) {
                        aggregate_edges[aggregate].push(nested);
                    }
                }
            }
        }

        let mut incoming = vec![0_usize; aggregate_edges.len()];
        for edges in &aggregate_edges {
            for target in edges {
                if let Some(count) = incoming.get_mut(*target) {
                    *count = count.saturating_add(1);
                }
            }
        }
        let mut pending = incoming
            .iter()
            .enumerate()
            .filter_map(|(index, count)| (*count == 0).then_some(index))
            .collect::<VecDeque<_>>();
        let mut visited = 0_usize;
        while let Some(aggregate) = pending.pop_front() {
            visited = visited.saturating_add(1);
            for target in &aggregate_edges[aggregate] {
                let Some(count) = incoming.get_mut(*target) else {
                    continue;
                };
                *count = count.saturating_sub(1);
                if *count == 0 {
                    pending.push_back(*target);
                }
            }
        }
        if visited == aggregate_edges.len() {
            for root in 0..aggregate_edges.len() {
                let mut pending = vec![(root, 1_usize)];
                let mut structural_nodes = 0_usize;
                let mut exceeded = false;
                while let Some((aggregate, depth)) = pending.pop() {
                    if depth > crate::repr::DIRECT_PRODUCT_MAX_NESTING_DEPTH {
                        exceeded = true;
                        break;
                    }
                    let Some(next_structural_nodes) =
                        structural_nodes.checked_add(aggregate_costs[aggregate])
                    else {
                        exceeded = true;
                        break;
                    };
                    if next_structural_nodes > crate::repr::DIRECT_PRODUCT_MAX_STRUCTURAL_NODES {
                        exceeded = true;
                        break;
                    }
                    structural_nodes = next_structural_nodes;
                    pending.extend(
                        aggregate_edges[aggregate]
                            .iter()
                            .copied()
                            .map(|child| (child, depth.saturating_add(1))),
                    );
                }
                if exceeded {
                    self.error(
                        ValidationCode::RepresentationPlan,
                        format!("representations.aggregate[{root}].structure"),
                        format!(
                            "direct aggregate closure exceeds the {}-node or {}-level structural budget",
                            crate::repr::DIRECT_PRODUCT_MAX_STRUCTURAL_NODES,
                            crate::repr::DIRECT_PRODUCT_MAX_NESTING_DEPTH
                        ),
                    );
                    break;
                }
            }
        } else {
            self.error(
                ValidationCode::RepresentationPlan,
                "representations.aggregates",
                "product and sum representation fields form a cycle",
            );
        }
        // Validate the semantic direct-value closure as well as the physical
        // product graph. Transparent aliases are representation-free, so a
        // deep refined chain would otherwise evade the same finite planning
        // budget enforced by source classification.
        for root in 0..representations.value_types().len() {
            let Some(root_id) = ValueTypeId::from_index(self.program.brand, root) else {
                break;
            };
            let mut pending = vec![(root_id, 1_usize)];
            let mut structural_nodes = 0_usize;
            let mut exceeded = false;
            while let Some((value, depth)) = pending.pop() {
                let Some(value_type) = representations.value_type(value) else {
                    continue;
                };
                let (transparent_base, aggregate_repr) = match value_type.kind() {
                    ValueTypeKind::Transparent { base } => (Some(base), None),
                    ValueTypeKind::Direct
                    | ValueTypeKind::ManagedTextMap
                    | ValueTypeKind::InvariantProduct => {
                        (None, representations.repr(value_type.repr()).copied())
                    }
                };
                let structural_cost = match (transparent_base, aggregate_repr) {
                    (Some(_), _) => Some(2),
                    (None, Some(Repr::Product(product))) => representations
                        .product(product)
                        .and_then(|product| 1_usize.checked_add(product.fields().len())),
                    (None, Some(Repr::Sum(sum))) => representations.sum(sum).and_then(|sum| {
                        sum.variants().iter().try_fold(
                            1_usize.checked_add(sum.variants().len())?,
                            |nodes, variant| nodes.checked_add(variant.fields().len()),
                        )
                    }),
                    (
                        None,
                        Some(
                            Repr::Uninhabited
                            | Repr::Zst
                            | Repr::Scalar(_)
                            | Repr::ImmortalText
                            | Repr::ManagedPointer
                            | Repr::TaskHandle,
                        )
                        | None,
                    ) => None,
                };
                let Some(structural_cost) = structural_cost else {
                    continue;
                };
                if depth > crate::repr::DIRECT_PRODUCT_MAX_NESTING_DEPTH {
                    exceeded = true;
                    break;
                }
                let Some(next_structural_nodes) = structural_nodes.checked_add(structural_cost)
                else {
                    exceeded = true;
                    break;
                };
                if next_structural_nodes > crate::repr::DIRECT_PRODUCT_MAX_STRUCTURAL_NODES {
                    exceeded = true;
                    break;
                }
                structural_nodes = next_structural_nodes;
                let mut queue_dependency = |dependency| {
                    let dependency_type = representations.value_type(dependency)?;
                    let nested = matches!(
                        dependency_type.kind(),
                        ValueTypeKind::Transparent { .. } | ValueTypeKind::InvariantProduct
                    ) || matches!(
                        representations.repr(dependency_type.repr()),
                        Some(Repr::Product(_) | Repr::Sum(_))
                    );
                    nested.then_some((dependency, depth.saturating_add(1)))
                };
                if let Some(base) = transparent_base
                    && let Some(dependency) = queue_dependency(base)
                {
                    pending.push(dependency);
                }
                match aggregate_repr {
                    Some(Repr::Product(product)) => {
                        if let Some(fields) = representations
                            .product(product)
                            .map(crate::ProductRepr::fields)
                        {
                            pending
                                .extend(fields.iter().copied().filter_map(&mut queue_dependency));
                        }
                    }
                    Some(Repr::Sum(sum)) => {
                        if let Some(sum) = representations.sum(sum) {
                            pending.extend(
                                sum.variants()
                                    .iter()
                                    .flat_map(|variant| variant.fields().iter().copied())
                                    .filter_map(&mut queue_dependency),
                            );
                        }
                    }
                    Some(
                        Repr::Uninhabited
                        | Repr::Zst
                        | Repr::Scalar(_)
                        | Repr::ImmortalText
                        | Repr::ManagedPointer
                        | Repr::TaskHandle,
                    )
                    | None => {}
                }
            }
            if exceeded {
                self.error(
                    ValidationCode::RepresentationPlan,
                    format!("representations.type[{root}].structure"),
                    format!(
                        "direct semantic value closure exceeds the {}-node or {}-level structural budget",
                        crate::repr::DIRECT_PRODUCT_MAX_STRUCTURAL_NODES,
                        crate::repr::DIRECT_PRODUCT_MAX_NESTING_DEPTH
                    ),
                );
                break;
            }
        }
        for (index, value_type) in self
            .program
            .representations
            .value_types()
            .iter()
            .enumerate()
        {
            if self
                .program
                .representations
                .repr(value_type.repr())
                .is_none()
            {
                self.error(
                    ValidationCode::InvalidTypeReference,
                    format!("representations.type[{index}]"),
                    format!(
                        "value type references missing representation {}",
                        value_type.repr()
                    ),
                );
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn validate_function(&mut self, function: &Function, function_index: usize) {
        let base = format!("function[{function_index}]");
        self.validate_structural_equality_role(function, &base);
        self.validate_signature(function, &base);
        self.validate_coroutine_plan(function, &base);
        let exact_effects = self
            .exact_effects
            .get(function_index)
            .copied()
            .unwrap_or(Effects::NONE);
        let closed_effects = function.effects.with_implications();
        if function.effects != closed_effects {
            self.error(
                ValidationCode::EffectImplication,
                format!("{base}.effects"),
                format!(
                    "function declares {}, but capability implications require {}",
                    function.effects, closed_effects
                ),
            );
        }
        if function.effects != exact_effects {
            self.error(
                ValidationCode::EffectMismatch,
                format!("{base}.effects"),
                format!(
                    "function declares {}, but its body and transitive callees require exactly {}",
                    function.effects, exact_effects
                ),
            );
        }

        for (index, block) in function.blocks.iter().enumerate() {
            if block.id.owner() != function.id || block.id.index() != index {
                self.error(
                    ValidationCode::IndexMismatch,
                    format!("{base}.block[{index}]"),
                    format!("block table index {index} carries identity {}", block.id),
                );
            }
        }
        for (index, instruction) in function.instructions.iter().enumerate() {
            if instruction.id.owner() != function.id || instruction.id.index() != index {
                self.error(
                    ValidationCode::IndexMismatch,
                    format!("{base}.instruction[{index}]"),
                    format!(
                        "instruction table index {index} carries identity {}",
                        instruction.id
                    ),
                );
            }
        }
        for (index, value) in function.values.iter().enumerate() {
            if value.id.owner() != function.id || value.id.index() != index {
                self.error(
                    ValidationCode::IndexMismatch,
                    format!("{base}.value[{index}]"),
                    format!("value table index {index} carries identity {}", value.id),
                );
            }
            self.require_type(value.ty, format!("{base}.value[{index}].type"));
            self.require_inhabited_type(value.ty, format!("{base}.value[{index}].type"));
        }
        let schedule = self.validate_schedule(function, &base);
        self.validate_value_definitions(function, &base);
        self.validate_entry(function, &base);

        let mut successors = vec![Vec::new(); function.blocks.len()];
        let mut predecessors = vec![Vec::new(); function.blocks.len()];
        for block_index in 0..function.blocks.len() {
            self.validate_block(
                function,
                block_index,
                &base,
                &mut successors,
                &mut predecessors,
            );
        }

        let Some(entry) = function
            .entry
            .filter(|entry| function.block(*entry).is_some())
        else {
            return;
        };
        if predecessors
            .get(entry.index())
            .is_some_and(|incoming| !incoming.is_empty())
        {
            self.error(
                ValidationCode::EntryPredecessor,
                format!("{base}.entry"),
                "the entry block cannot have a CFG predecessor; use a separate loop header",
            );
        }
        let reachable = reachable_blocks(entry.index(), &successors);
        let cancellation_states = compute_cancellation_states(function);
        for (index, is_reachable) in reachable.iter().copied().enumerate() {
            if !is_reachable {
                self.error(
                    ValidationCode::UnreachableBlock,
                    format!("{base}.block[{index}]"),
                    "block is not reachable from the function entry",
                );
            }
        }
        for index in 0..function.blocks.len() {
            let state = self
                .fault_states
                .get(function_index)
                .and_then(|states| states.get(index))
                .copied()
                .unwrap_or(FaultStateSet::NONE);
            if state == FaultStateSet::BOTH {
                self.error(
                    ValidationCode::FaultState,
                    format!("{base}.block[{index}]"),
                    "block is reachable with both inactive and active source-fault state; split the control-flow merge",
                );
            }
            if let Some(block) = function.blocks.get(index)
                && let Some(terminator) = block.terminator.as_ref()
            {
                self.validate_terminator_fault_state(terminator, state, index, &base);
                let cancellation_state = cancellation_states
                    .get(index)
                    .copied()
                    .unwrap_or(CancellationStateSet::NONE);
                // Once a cleanup itself faults, source-fault propagation wins
                // over the distinction between an ordinary child fault and a
                // fault raised while cancelling. Those paths may share the
                // canonical resume_fault tail. Without an active fault the
                // cancellation obligation must remain distinct.
                if cancellation_state == CancellationStateSet::BOTH
                    && state != FaultStateSet::ACTIVE
                {
                    self.error(
                        ValidationCode::InvalidCoroutinePlan,
                        format!("{base}.block[{index}]"),
                        "block is reachable from both ordinary and cancellation continuations; split the control-flow merge",
                    );
                }
                self.validate_block_cancellation_state(
                    function,
                    block,
                    terminator,
                    cancellation_state,
                    index,
                    &base,
                );
            }
        }
        let dominators = compute_dominators(entry.index(), &reachable, &successors, &predecessors);
        self.validate_dominance(function, &base, &schedule, &reachable, &dominators);
        self.validate_integer_proofs(function, &base, &reachable, &predecessors, &dominators);
        self.validate_list_uniqueness(function, &base, &reachable);
        self.validate_task_ownership(function, &base, &reachable);
    }

    fn validate_structural_equality_role(&mut self, function: &Function, base: &str) {
        let Some(key) = self.program.instances.key(function.id()) else {
            return;
        };
        if key.role() != InstanceRole::StructuralEquality {
            return;
        }
        let compared = key
            .structural_equality_type()
            .and_then(|semantic| self.program.representations.type_id(semantic));
        let boolean = self.program.representations.type_id(&Type::Bool);
        let exact_signature = compared.is_some_and(|compared| {
            function.signature.params() == [compared, compared]
                && Some(function.signature.result()) == boolean
                && function.signature.inout_params().is_empty()
        });
        if !exact_signature {
            self.error(
                ValidationCode::InstancePlan,
                format!("{base}.signature"),
                "structural-equality helper requires one closed registered key type and exact (T, T) -> Bool signature without inout parameters",
            );
        }
        if function.effects != Effects::NONE {
            self.error(
                ValidationCode::InstancePlan,
                format!("{base}.effects"),
                "structural-equality helper must be effect-free",
            );
        }
        if function.coroutine.is_some() {
            self.error(
                ValidationCode::InstancePlan,
                format!("{base}.coroutine"),
                "structural-equality helper cannot be a coroutine",
            );
        }
        if function.origin != Origin::synthetic(key.source()) {
            self.error(
                ValidationCode::InstancePlan,
                format!("{base}.origin"),
                "structural-equality helper requires a synthetic origin",
            );
        }
    }

    fn validate_task_ownership(&mut self, function: &Function, base: &str, reachable: &[bool]) {
        let task_values = function
            .values()
            .iter()
            .map(|value| {
                representation_contains_task_handle(&self.program.representations, value.ty())
                    == Some(true)
            })
            .collect::<Vec<_>>();
        if !task_values.iter().any(|is_task| *is_task) {
            return;
        }

        let empty = vec![TaskAvailability::NONE; function.values().len()];
        let mut inputs = vec![empty.clone(); function.blocks().len()];
        let Some(entry) = function.entry() else {
            return;
        };
        for parameter in function
            .block(entry)
            .into_iter()
            .flat_map(crate::Block::params)
            .copied()
            .filter(|value| task_values.get(value.index()).copied().unwrap_or(false))
        {
            inputs[entry.index()][parameter.index()] = TaskAvailability::AVAILABLE;
        }

        loop {
            let mut next = vec![empty.clone(); function.blocks().len()];
            let mut seen = vec![false; function.blocks().len()];
            next[entry.index()].clone_from(&inputs[entry.index()]);
            seen[entry.index()] = true;
            for (block_index, block) in function.blocks().iter().enumerate() {
                if !reachable.get(block_index).copied().unwrap_or(false) {
                    continue;
                }
                for edge in transfer_task_ownership(
                    function,
                    block,
                    &inputs[block_index],
                    &task_values,
                    false,
                )
                .edges
                {
                    if edge.target == entry.index()
                        || !reachable.get(edge.target).copied().unwrap_or(false)
                    {
                        continue;
                    }
                    if seen[edge.target] {
                        for (current, incoming) in next[edge.target]
                            .iter_mut()
                            .zip(edge.states.iter().copied())
                        {
                            *current = current.union(incoming);
                        }
                    } else {
                        next[edge.target] = edge.states;
                        seen[edge.target] = true;
                    }
                }
            }
            if next == inputs {
                break;
            }
            inputs = next;
        }

        let mut reported = BTreeSet::new();
        for (block_index, block) in function.blocks().iter().enumerate() {
            if !reachable.get(block_index).copied().unwrap_or(false) {
                continue;
            }
            let transfer =
                transfer_task_ownership(function, block, &inputs[block_index], &task_values, true);
            for issue in transfer.issues {
                if reported.insert((issue.site, issue.value, issue.message)) {
                    let path = match issue.site {
                        TaskOwnershipSite::Instruction(instruction) => {
                            format!("{base}.instruction[{}].task", instruction.index())
                        }
                        TaskOwnershipSite::Terminator(block) => {
                            format!("{base}.block[{}].terminator.task", block.index())
                        }
                    };
                    self.error(ValidationCode::InvalidTaskOwnership, path, issue.message);
                }
            }
        }
    }

    fn validate_list_uniqueness(&mut self, function: &Function, base: &str, reachable: &[bool]) {
        let list_values = function
            .values()
            .iter()
            .map(|value| {
                self.program
                    .representations
                    .value_type(value.ty())
                    .is_some_and(|ty| matches!(ty.semantic(), Type::List(_)))
            })
            .collect::<Vec<_>>();
        if !function.instructions().iter().any(|instruction| {
            matches!(instruction.kind(), InstructionKind::ListAppendUnique { .. })
        }) {
            return;
        }

        let top = list_values
            .iter()
            .map(|is_list| {
                if *is_list {
                    ListOwnership::Unique
                } else {
                    ListOwnership::Shared
                }
            })
            .collect::<Vec<_>>();
        let mut inputs = vec![top.clone(); function.blocks().len()];
        let Some(entry) = function.entry() else {
            return;
        };
        for parameter in function
            .block(entry)
            .into_iter()
            .flat_map(crate::Block::params)
            .copied()
            .filter(|value| list_values.get(value.index()).copied().unwrap_or(false))
        {
            inputs[entry.index()][parameter.index()] = ListOwnership::Shared;
        }

        loop {
            let mut next = vec![top.clone(); function.blocks().len()];
            let mut seen = vec![false; function.blocks().len()];
            next[entry.index()].clone_from(&inputs[entry.index()]);
            seen[entry.index()] = true;
            for (block_index, block) in function.blocks().iter().enumerate() {
                if !reachable.get(block_index).copied().unwrap_or(false) {
                    continue;
                }
                for edge in transfer_list_ownership(
                    function,
                    block,
                    &inputs[block_index],
                    &list_values,
                    false,
                )
                .edges
                {
                    if edge.target == entry.index()
                        || !reachable.get(edge.target).copied().unwrap_or(false)
                    {
                        continue;
                    }
                    if seen[edge.target] {
                        for (current, incoming) in next[edge.target]
                            .iter_mut()
                            .zip(edge.states.iter().copied())
                        {
                            *current = current.meet(incoming);
                        }
                    } else {
                        next[edge.target] = edge.states;
                        seen[edge.target] = true;
                    }
                }
            }
            if next == inputs {
                break;
            }
            inputs = next;
        }

        let mut reported = BTreeSet::new();
        for (block_index, block) in function.blocks().iter().enumerate() {
            if !reachable.get(block_index).copied().unwrap_or(false) {
                continue;
            }
            let transfer =
                transfer_list_ownership(function, block, &inputs[block_index], &list_values, true);
            for issue in transfer.issues {
                if reported.insert((issue.instruction, issue.value, issue.message)) {
                    self.error(
                        ValidationCode::InvalidListUniqueness,
                        format!("{base}.instruction[{}].list", issue.instruction.index()),
                        issue.message,
                    );
                }
            }
        }
    }

    fn validate_signature(&mut self, function: &Function, base: &str) {
        for (index, ty) in function.signature.params().iter().copied().enumerate() {
            self.require_type(ty, format!("{base}.signature.param[{index}]"));
            self.require_inhabited_type(ty, format!("{base}.signature.param[{index}]"));
        }
        self.require_type(
            function.signature.result(),
            format!("{base}.signature.result"),
        );
        self.require_inhabited_type(
            function.signature.result(),
            format!("{base}.signature.result"),
        );
        let mut previous = None;
        for (writeback_index, parameter) in function
            .signature
            .inout_params()
            .iter()
            .copied()
            .enumerate()
        {
            if previous.is_some_and(|previous| previous >= parameter) {
                self.error(
                    ValidationCode::InOutShape,
                    format!("{base}.signature.inout[{writeback_index}]"),
                    "inout parameter positions must be strictly increasing",
                );
            }
            previous = Some(parameter);
            let Some(ty) = usize::try_from(parameter)
                .ok()
                .and_then(|index| function.signature.params().get(index))
                .copied()
            else {
                self.error(
                    ValidationCode::InOutShape,
                    format!("{base}.signature.inout[{writeback_index}]"),
                    format!("inout parameter position {parameter} is out of range"),
                );
                continue;
            };
            if self.product_fields(ty).is_none()
                && self.program.representations.dynamic(ty).is_none()
            {
                self.error(
                    ValidationCode::InOutShape,
                    format!("{base}.signature.inout[{writeback_index}]"),
                    format!(
                        "inout parameter {parameter} must use a direct product or closed dynamic value type"
                    ),
                );
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn validate_coroutine_plan(&mut self, function: &Function, base: &str) {
        let Some(plan) = function.coroutine() else {
            if function.blocks().iter().any(|block| {
                matches!(
                    block.terminator().map(crate::Terminator::kind),
                    Some(TerminatorKind::AwaitTasks { .. })
                )
            }) {
                self.error(
                    ValidationCode::InvalidCoroutinePlan,
                    format!("{base}.coroutine"),
                    "await_tasks is only valid in a function with a checked coroutine plan",
                );
            }
            return;
        };

        let has_caller_blame_precondition = function.blocks().iter().any(|block| {
            let metadata = match block.terminator().map(Terminator::kind) {
                Some(
                    TerminatorKind::Assert { metadata, .. } | TerminatorKind::Fault { metadata },
                ) => Some(metadata),
                _ => None,
            };
            matches!(
                metadata,
                Some(crate::FaultMetadata::Contract(metadata))
                    if metadata.kind() == crate::ContractFaultKind::Precondition
                        && metadata.blame()
                            == crate::ContractFaultBlame::CoroutineCallSite
            )
        });
        if plan.carries_caller_span() != has_caller_blame_precondition {
            self.error(
                ValidationCode::InvalidCoroutinePlan,
                format!("{base}.coroutine.caller_span"),
                if plan.carries_caller_span() {
                    "a coroutine may carry its caller span only when at least one precondition uses coroutine-call-site blame"
                } else {
                    "a coroutine precondition using coroutine-call-site blame requires its plan to carry the caller span"
                },
            );
        }
        if plan.carries_caller_span() {
            self.validate_contract_fault_span(
                function.origin().span,
                &format!("{base}.origin.span"),
            );
        }

        if plan.output() != function.signature().result() {
            self.error(
                ValidationCode::InvalidCoroutinePlan,
                format!("{base}.coroutine.output"),
                "coroutine output must exactly match the function result type",
            );
        }
        let canonical_output = self
            .program
            .representations
            .value_type(plan.output())
            .and_then(|output| self.program.representations.type_id(output.semantic()));
        if canonical_output != Some(plan.output()) {
            self.error(
                ValidationCode::InvalidCoroutinePlan,
                format!("{base}.coroutine.output"),
                "coroutine output must use its canonical value representation at the Task ABI boundary",
            );
        }
        if !function.signature().inout_params().is_empty() {
            self.error(
                ValidationCode::InvalidCoroutinePlan,
                format!("{base}.signature.inout"),
                "typed coroutine plans do not admit inout parameters",
            );
        }
        if !function.effects().contains(Effects::NEEDS_EXECUTOR) {
            self.error(
                ValidationCode::InvalidCoroutinePlan,
                format!("{base}.effects"),
                "a coroutine requires the executor capability",
            );
        }

        for (index, ty) in function
            .signature()
            .params()
            .iter()
            .copied()
            .chain(std::iter::once(plan.output()))
            .enumerate()
        {
            if !self.coroutine_frame_type_supported(ty, false) {
                self.error(
                    ValidationCode::InvalidCoroutinePlan,
                    format!("{base}.coroutine.frame_type[{index}]"),
                    "typed coroutine parameter/result slots require closed direct values or cataloged dynamic pointers without task handles",
                );
            }
        }

        let mut await_states = BTreeMap::<
            u32,
            (
                AwaitMode,
                &ResultTarget,
                &UnwindTarget,
                &crate::BlockTarget,
                &[ValueId],
            ),
        >::new();
        for block in function.blocks() {
            if let Some(TerminatorKind::AwaitTasks {
                state,
                mode,
                tasks,
                normal,
                fault,
                cancel,
            }) = block.terminator().map(crate::Terminator::kind)
                && await_states
                    .insert(*state, (*mode, normal, fault, cancel, tasks))
                    .is_some()
            {
                self.error(
                    ValidationCode::InvalidCoroutinePlan,
                    format!("{base}.coroutine.state[{state}]"),
                    "each coroutine resume state must have exactly one await_tasks terminator",
                );
            }
        }

        let mut previous = 0_u32;
        for (index, suspension) in plan.suspensions().iter().enumerate() {
            let expected_state = u32::try_from(index)
                .ok()
                .and_then(|index| index.checked_add(1));
            if suspension.state() == 0
                || suspension.state() <= previous
                || Some(suspension.state()) != expected_state
            {
                self.error(
                    ValidationCode::InvalidCoroutinePlan,
                    format!("{base}.coroutine.suspension[{index}].state"),
                    "coroutine resume states must be the dense ordered sequence 1..n",
                );
            }
            previous = suspension.state();
            if suspension.awaited().is_empty() {
                self.error(
                    ValidationCode::InvalidCoroutinePlan,
                    format!("{base}.coroutine.suspension[{index}].awaited"),
                    "a coroutine suspension must await at least one child result",
                );
            }
            for (awaited_index, ty) in suspension.awaited().iter().copied().enumerate() {
                if !self.coroutine_frame_type_supported(ty, false) {
                    self.error(
                        ValidationCode::InvalidCoroutinePlan,
                        format!(
                            "{base}.coroutine.suspension[{index}].awaited[{awaited_index}]"
                        ),
                        "typed coroutine awaited-result slots require closed direct values or cataloged dynamic pointers without task handles",
                    );
                }
            }
            for (live_index, ty) in suspension.live().iter().copied().enumerate() {
                if !self.coroutine_frame_type_supported(ty, true) {
                    self.error(
                        ValidationCode::InvalidCoroutinePlan,
                        format!("{base}.coroutine.suspension[{index}].live[{live_index}]"),
                        "typed coroutine live slots require supported closed by-value carriers",
                    );
                }
            }
            let Some((mode, normal, fault, cancel, tasks)) =
                await_states.remove(&suspension.state())
            else {
                self.error(
                    ValidationCode::InvalidCoroutinePlan,
                    format!("{base}.coroutine.suspension[{index}]"),
                    "coroutine plan row has no matching await_tasks terminator",
                );
                continue;
            };
            if mode != suspension.mode() {
                self.error(
                    ValidationCode::InvalidCoroutinePlan,
                    format!("{base}.coroutine.suspension[{index}].mode"),
                    "coroutine plan join mode does not match its await_tasks terminator",
                );
            }
            if tasks.len() != suspension.awaited().len() {
                self.error(
                    ValidationCode::InvalidCoroutinePlan,
                    format!("{base}.coroutine.suspension[{index}].awaited"),
                    format!(
                        "coroutine plan has {} awaited result slot(s), await_tasks consumes {} task(s)",
                        suspension.awaited().len(),
                        tasks.len()
                    ),
                );
            }
            for (awaited_index, (task, expected)) in tasks
                .iter()
                .copied()
                .zip(suspension.awaited().iter().copied())
                .enumerate()
            {
                let actual = self.task_output_type(function, task);
                if actual != Some(expected) {
                    self.error(
                        ValidationCode::InvalidCoroutinePlan,
                        format!("{base}.coroutine.suspension[{index}].awaited[{awaited_index}]"),
                        "coroutine awaited-result type does not match its child Task output",
                    );
                }
            }
            if normal.arguments.len() != suspension.live().len() {
                self.error(
                    ValidationCode::InvalidCoroutinePlan,
                    format!("{base}.coroutine.suspension[{index}].live"),
                    format!(
                        "coroutine plan has {} live slot(s), await_tasks forwards {}",
                        suspension.live().len(),
                        normal.arguments.len()
                    ),
                );
            }
            if fault.arguments.as_ref() != normal.arguments.as_ref()
                || cancel.arguments.as_ref() != normal.arguments.as_ref()
            {
                self.error(
                    ValidationCode::InvalidCoroutinePlan,
                    format!("{base}.coroutine.suspension[{index}].live"),
                    "await_tasks normal, fault, and cancel exits must forward the same exact live-value row",
                );
            }
            for (live_index, (argument, expected)) in normal
                .arguments
                .iter()
                .copied()
                .zip(suspension.live().iter().copied())
                .enumerate()
            {
                self.require_value_type(
                    function,
                    argument,
                    expected,
                    ValidationCode::InvalidCoroutinePlan,
                    format!("{base}.coroutine.suspension[{index}].live[{live_index}]"),
                );
            }
        }
        for state in await_states.keys() {
            self.error(
                ValidationCode::InvalidCoroutinePlan,
                format!("{base}.coroutine.state[{state}]"),
                "await_tasks terminator has no matching coroutine-plan row",
            );
        }
    }

    /// Validates the complete compiler/runtime contract for consuming one
    /// terminal typed child. The nominal ids and ordered physical fields are
    /// intentionally rechecked here: a target backend may construct the sum
    /// directly only after this boundary proves that it is the canonical
    /// `TaskOutcome[T]` backed by the canonical `TaskFault` and managed Text.
    #[allow(clippy::too_many_lines)]
    fn validate_task_outcome_take(
        &mut self,
        function: &Function,
        instruction: &Instruction,
        task: ValueId,
        path: &str,
    ) {
        if !Self::task_outcome_take_has_terminal_provenance(function, instruction, task) {
            self.error(
                ValidationCode::InvalidTaskOwnership,
                format!("{path}.task"),
                "task.outcome_take operand must be a leading normal block parameter produced by exactly one settled or race await_tasks edge",
            );
        }
        let output = self.task_output_type(function, task);
        if function.value(task).is_some() && output.is_none() {
            self.error(
                ValidationCode::TypeMismatch,
                format!("{path}.task"),
                "task.outcome_take operand must be a canonical concrete Task handle",
            );
        }
        let output_semantic = output.and_then(|output| {
            self.program
                .representations
                .value_type(output)
                .map(crate::ValueType::semantic)
                .cloned()
        });
        let outcome_semantic = self
            .program
            .canonical_types
            .task_outcome
            .zip(output_semantic.as_ref())
            .map(|(outcome, output)| Type::Nominal(outcome, vec![output.clone()]));
        let expected_outcome = outcome_semantic
            .as_ref()
            .and_then(|semantic| self.program.representations.type_id(semantic));
        if output_semantic.is_some() && expected_outcome.is_none() {
            self.error(
                ValidationCode::TypeMismatch,
                format!("{path}.result[0]"),
                "task.outcome_take requires the cataloged canonical TaskOutcome[T] result type",
            );
        }
        self.require_results(function, instruction, &[expected_outcome], path);

        let result_ty = instruction
            .results()
            .first()
            .and_then(|result| function.value(*result))
            .map(Value::ty);
        if let (Some(result_ty), Some(outcome_semantic)) = (result_ty, outcome_semantic.as_ref()) {
            let canonical_result = self
                .program
                .representations
                .value_type(result_ty)
                .is_some_and(|result| {
                    result.kind() == ValueTypeKind::Direct
                        && result.semantic() == outcome_semantic
                        && self.program.representations.type_id(result.semantic())
                            == Some(result_ty)
                });
            if !canonical_result {
                self.error(
                    ValidationCode::TypeMismatch,
                    format!("{path}.result[0]"),
                    "task.outcome_take result must use the cataloged canonical direct TaskOutcome[T] value type",
                );
            }
        }

        let text = self.scalar_type(&Type::Text);
        let managed_text = text.is_some_and(|text| {
            self.program
                .representations
                .value_type(text)
                .is_some_and(|value| {
                    value.kind() == ValueTypeKind::Direct
                        && value.semantic() == &Type::Text
                        && self.program.representations.repr(value.repr())
                            == Some(&Repr::ManagedPointer)
                })
        });
        if !managed_text {
            self.error(
                ValidationCode::TypeMismatch,
                format!("{path}.task_fault.text"),
                "task.outcome_take requires the canonical managed Text representation",
            );
        }

        let fault_semantic = self
            .program
            .canonical_types
            .task_fault
            .map(|fault| Type::Nominal(fault, Vec::new()));
        let fault = fault_semantic
            .as_ref()
            .and_then(|semantic| self.program.representations.type_id(semantic));
        let fault_fields = fault.and_then(|fault| {
            let value = self.program.representations.value_type(fault)?;
            (value.kind() == ValueTypeKind::Direct
                && Some(value.semantic()) == fault_semantic.as_ref()
                && self.program.representations.type_id(value.semantic()) == Some(fault))
            .then(|| self.product_fields(fault).map(<[ValueTypeId]>::to_vec))
            .flatten()
        });
        let expected_fault_fields = text.map(|text| vec![text, text]);
        if fault_fields != expected_fault_fields {
            self.error(
                ValidationCode::TypeMismatch,
                format!("{path}.task_fault"),
                "task.outcome_take requires cataloged TaskFault as direct product (Text, Text)",
            );
        }

        if let Some(outcome) = expected_outcome {
            let variants = self.sum_repr(outcome).and_then(|sum| {
                self.program.representations.sum(sum).map(|sum| {
                    sum.variants()
                        .iter()
                        .map(|variant| variant.fields().to_vec())
                        .collect::<Vec<_>>()
                })
            });
            let completed = usize::try_from(TASK_OUTCOME_COMPLETED_VARIANT).ok();
            let faulted = usize::try_from(TASK_OUTCOME_FAULTED_VARIANT).ok();
            let cancelled = usize::try_from(TASK_OUTCOME_CANCELLED_VARIANT).ok();
            let expected_completed = output.map(|output| vec![output]);
            let expected_faulted = fault.map(|fault| vec![fault]);
            let exact_variants = variants.as_ref().is_some_and(|variants| {
                variants.len() == 3
                    && completed.and_then(|index| variants.get(index))
                        == expected_completed.as_ref()
                    && faulted.and_then(|index| variants.get(index)) == expected_faulted.as_ref()
                    && cancelled
                        .and_then(|index| variants.get(index))
                        .is_some_and(Vec::is_empty)
            });
            if !exact_variants {
                self.error(
                    ValidationCode::InstructionShape,
                    format!("{path}.result[0]"),
                    "TaskOutcome must have exactly ordered variants Completed(T), Faulted(TaskFault), Cancelled",
                );
            }
        }

        if function.coroutine().is_none() {
            self.error(
                ValidationCode::InvalidCoroutinePlan,
                path,
                "task.outcome_take requires an active typed-coroutine executor context",
            );
        }
        if !function.effects().contains(Effects::MAY_COLLECT) {
            self.error(
                ValidationCode::EffectMismatch,
                path,
                "task.outcome_take requires the function's MAY_COLLECT effect",
            );
        }
        if !function.effects().contains(Effects::NEEDS_EXECUTOR) {
            self.error(
                ValidationCode::EffectMismatch,
                path,
                "task.outcome_take requires the function's NEEDS_EXECUTOR effect",
            );
        }
    }

    /// Keeps terminal child ownership linear at the settled/race resume
    /// boundary. The leading implicit normal parameters are runtime-owned
    /// terminal capabilities, whereas every following parameter is an ordinary
    /// forwarded live value. Requiring the takes as an ordered instruction
    /// prefix makes it impossible for checked LCIR to drop a terminal child,
    /// delay its retirement, or clear it from a later join row before capture;
    /// the general affine Task transfer separately rejects every repeated take.
    fn validate_terminal_task_take_prefix(
        &mut self,
        function: &Function,
        mode: AwaitMode,
        implicit_results: usize,
        normal: &ResultTarget,
        path: &str,
    ) {
        if !matches!(mode, AwaitMode::Settled | AwaitMode::Race) {
            return;
        }
        let Some(block) = function.block(normal.block) else {
            return;
        };
        for index in 0..implicit_results {
            let expected = block.params().get(index).copied();
            let actual = block
                .instructions()
                .get(index)
                .and_then(|instruction| function.instruction(*instruction))
                .and_then(|instruction| match instruction.kind() {
                    InstructionKind::TaskOutcomeTake { task } => Some(*task),
                    _ => None,
                });
            if expected.is_none() || actual != expected {
                self.error(
                    ValidationCode::InvalidTaskOwnership,
                    format!("{path}.normal.take[{index}]"),
                    "settled/race normal blocks must begin with one task.outcome_take for every terminal Task result in exact parameter order",
                );
            }
        }
    }

    /// Proves the capability carried by the otherwise ordinary `Task[T]`
    /// operand. Exactly one incoming edge must define the parameter, that edge
    /// must be the normal edge of `settled` or `race`, and the take must remain
    /// in that dedicated normal block. This rejects arbitrary created, returned,
    /// or forwarded handles whose tasks may still be pending at runtime.
    fn task_outcome_take_has_terminal_provenance(
        function: &Function,
        instruction: &Instruction,
        task: ValueId,
    ) -> bool {
        let Some(value) = function.value(task) else {
            return false;
        };
        let ValueDefinition::BlockParameter { block, index } = value.definition() else {
            return false;
        };
        let Ok(index) = usize::try_from(index) else {
            return false;
        };
        if !function
            .block(block)
            .is_some_and(|block| block.instructions().contains(&instruction.id()))
        {
            return false;
        }

        let mut incoming = 0_usize;
        let mut terminal_producer = false;
        for source in function.blocks() {
            let Some(terminator) = source.terminator() else {
                continue;
            };
            incoming = incoming.saturating_add(
                terminator
                    .control_flow_edges()
                    .into_iter()
                    .filter(|edge| edge.block == block)
                    .count(),
            );
            let TerminatorKind::AwaitTasks {
                mode,
                tasks,
                normal,
                ..
            } = terminator.kind()
            else {
                continue;
            };
            if normal.block != block {
                continue;
            }
            let source_task = match mode {
                AwaitMode::Settled => tasks.get(index),
                AwaitMode::Race if index == 0 => tasks.first(),
                AwaitMode::All | AwaitMode::Any | AwaitMode::Race => None,
            };
            terminal_producer = source_task
                .and_then(|task| function.value(*task))
                .is_some_and(|source| source.ty() == value.ty());
        }
        incoming == 1 && terminal_producer
    }

    fn task_output_type(&self, function: &Function, task: ValueId) -> Option<ValueTypeId> {
        function
            .value(task)
            .map(crate::Value::ty)
            .and_then(|task_id| {
                let task = self.program.representations.value_type(task_id)?;
                (self.program.representations.repr(task.repr()) == Some(&Repr::TaskHandle)
                    && self.program.representations.type_id(task.semantic()) == Some(task_id))
                .then_some(task.semantic())
            })
            .and_then(|semantic| match semantic {
                Type::Task(output) => self.program.representations.type_id(output),
                _ => None,
            })
    }

    fn coroutine_frame_type_supported(&self, root: ValueTypeId, allow_task_handle: bool) -> bool {
        let mut pending = vec![(root, allow_task_handle)];
        let mut seen = BTreeSet::new();
        while let Some((ty, task_handle_allowed)) = pending.pop() {
            if !seen.insert((ty, task_handle_allowed)) {
                continue;
            }
            let Some(value_type) = self.program.representations.value_type(ty) else {
                return false;
            };
            match self.program.representations.repr(value_type.repr()) {
                Some(Repr::Zst | Repr::Scalar(_) | Repr::ImmortalText) => {}
                Some(Repr::ManagedPointer) => {
                    let canonical =
                        self.program.representations.type_id(value_type.semantic()) == Some(ty);
                    if !canonical || !managed_pointer_semantic_is_valid(self.program, ty) {
                        return false;
                    }
                }
                Some(Repr::TaskHandle) => {
                    if !task_handle_allowed {
                        return false;
                    }
                }
                Some(Repr::Product(product)) => {
                    let Some(product) = self.program.representations.product(*product) else {
                        return false;
                    };
                    pending.extend(
                        product
                            .fields()
                            .iter()
                            .copied()
                            .map(|field| (field, task_handle_allowed)),
                    );
                }
                Some(Repr::Sum(sum)) => {
                    let Some(sum) = self.program.representations.sum(*sum) else {
                        return false;
                    };
                    pending.extend(
                        sum.variants()
                            .iter()
                            .flat_map(|variant| variant.fields().iter().copied())
                            .map(|field| (field, task_handle_allowed)),
                    );
                }
                Some(Repr::Uninhabited) | None => return false,
            }
        }
        true
    }

    fn validate_schedule(
        &mut self,
        function: &Function,
        base: &str,
    ) -> Vec<Option<(BlockId, usize)>> {
        let mut schedule = vec![None; function.instructions.len()];
        for (block_index, block) in function.blocks.iter().enumerate() {
            let Some(canonical_block) = BlockId::from_index(function.id, block_index) else {
                continue;
            };
            for (position, instruction) in block.instructions.iter().copied().enumerate() {
                let path = format!("{base}.block[{block_index}].instruction[{position}]");
                if instruction.owner() != function.id {
                    self.error(
                        ValidationCode::InvalidInstructionReference,
                        path,
                        format!("scheduled instruction {instruction} belongs to another function"),
                    );
                    continue;
                }
                let Some(slot) = schedule.get_mut(instruction.index()) else {
                    self.error(
                        ValidationCode::InvalidInstructionReference,
                        path,
                        format!("scheduled instruction {instruction} does not exist"),
                    );
                    continue;
                };
                if let Some((previous, _)) = slot {
                    self.error(
                        ValidationCode::InstructionSchedule,
                        path,
                        format!(
                            "instruction {instruction} is scheduled in both {previous} and {}",
                            block.id
                        ),
                    );
                } else {
                    *slot = Some((canonical_block, position));
                }
            }
        }
        for (index, location) in schedule.iter().enumerate() {
            if location.is_none() {
                self.error(
                    ValidationCode::InstructionSchedule,
                    format!("{base}.instruction[{index}]"),
                    "instruction is not scheduled in any block",
                );
            }
        }
        schedule
    }

    fn validate_value_definitions(&mut self, function: &Function, base: &str) {
        for (block_index, block) in function.blocks.iter().enumerate() {
            let Some(canonical_block) = BlockId::from_index(function.id, block_index) else {
                continue;
            };
            for (index, value) in block.params.iter().copied().enumerate() {
                let path = format!("{base}.block[{block_index}].param[{index}]");
                let Some(definition) = function.value(value).map(|value| value.definition) else {
                    self.error(
                        ValidationCode::InvalidValueReference,
                        path,
                        format!("block parameter {value} does not exist"),
                    );
                    continue;
                };
                let expected = ValueDefinition::BlockParameter {
                    block: canonical_block,
                    index: u32::try_from(index).unwrap_or(u32::MAX),
                };
                if definition != expected {
                    self.error(
                        ValidationCode::ValueDefinition,
                        path,
                        format!("{value} has definition {definition:?}, expected {expected:?}"),
                    );
                }
            }
        }
        for (instruction_index, instruction) in function.instructions.iter().enumerate() {
            let Some(canonical_instruction) =
                InstructionId::from_index(function.id, instruction_index)
            else {
                continue;
            };
            for (index, value) in instruction.results.iter().copied().enumerate() {
                let path = format!("{base}.instruction[{instruction_index}].result[{index}]");
                let Some(definition) = function.value(value).map(|value| value.definition) else {
                    self.error(
                        ValidationCode::InvalidValueReference,
                        path,
                        format!("instruction result {value} does not exist"),
                    );
                    continue;
                };
                let expected = ValueDefinition::InstructionResult {
                    instruction: canonical_instruction,
                    index: u32::try_from(index).unwrap_or(u32::MAX),
                };
                if definition != expected {
                    self.error(
                        ValidationCode::ValueDefinition,
                        path,
                        format!("{value} has definition {definition:?}, expected {expected:?}"),
                    );
                }
            }
        }
        for (value_index, value) in function.values.iter().enumerate() {
            let valid = match value.definition {
                ValueDefinition::BlockParameter { block, index } => {
                    function
                        .block(block)
                        .and_then(|block| block.params.get(index as usize))
                        == Some(&value.id)
                }
                ValueDefinition::InstructionResult { instruction, index } => {
                    function
                        .instruction(instruction)
                        .and_then(|instruction| instruction.results.get(index as usize))
                        == Some(&value.id)
                }
            };
            if !valid {
                self.error(
                    ValidationCode::ValueDefinition,
                    format!("{base}.value[{value_index}].definition"),
                    format!("{} is not owned by its declared definition", value.id),
                );
            }
        }
    }

    fn validate_entry(&mut self, function: &Function, base: &str) {
        let Some(entry) = function.entry else {
            self.error(
                ValidationCode::MissingEntry,
                format!("{base}.entry"),
                "function has no entry block",
            );
            return;
        };
        let Some(block) = function.block(entry) else {
            self.error(
                ValidationCode::InvalidBlockReference,
                format!("{base}.entry"),
                format!("entry block {entry} does not exist"),
            );
            return;
        };
        if block.params.len() != function.signature.params().len() {
            self.error(
                ValidationCode::EntrySignature,
                format!("{base}.entry"),
                format!(
                    "entry has {} parameters, signature requires {}",
                    block.params.len(),
                    function.signature.params().len()
                ),
            );
        }
        for (index, (value, expected)) in block
            .params
            .iter()
            .copied()
            .zip(function.signature.params().iter().copied())
            .enumerate()
        {
            self.require_value_type(
                function,
                value,
                expected,
                ValidationCode::EntrySignature,
                format!("{base}.entry.param[{index}]"),
            );
        }
    }

    fn validate_block(
        &mut self,
        function: &Function,
        block_index: usize,
        base: &str,
        successors: &mut [Vec<usize>],
        predecessors: &mut [Vec<usize>],
    ) {
        let block = &function.blocks[block_index];
        for instruction in block.instructions.iter().copied() {
            if let Some(instruction) = function.instruction(instruction) {
                self.validate_instruction(function, instruction, base);
            }
        }
        let Some(terminator) = &block.terminator else {
            self.error(
                ValidationCode::MissingTerminator,
                format!("{base}.block[{block_index}].terminator"),
                "block has no terminator",
            );
            return;
        };
        self.validate_terminator(function, block_index, terminator, base);
        for edge in terminator.control_flow_edges() {
            if function.block(edge.block).is_none() {
                continue;
            }
            successors[block_index].push(edge.block.index());
            if let Some(incoming) = predecessors.get_mut(edge.block.index()) {
                incoming.push(block_index);
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn validate_instruction(&mut self, function: &Function, instruction: &Instruction, base: &str) {
        let path = format!("{base}.instruction[{}]", instruction.id.index());
        if instruction.origin.source_function != function.source() {
            self.error(
                ValidationCode::OriginMismatch,
                format!("{path}.origin"),
                format!(
                    "origin names source function f{}, expected f{}",
                    instruction.origin.source_function.0,
                    function.source().0
                ),
            );
        }
        let unit = self.scalar_type(&Type::Unit);
        let boolean = self.scalar_type(&Type::Bool);
        let integer = self.scalar_type(&Type::Int);
        let float = self.scalar_type(&Type::Float);
        let integer_pair = self.scalar_type(&Type::Tuple(vec![Type::Int, Type::Int]));
        let text = self.scalar_type(&Type::Text);
        let bytes = self.managed_bytes_type();
        let text_is_managed = text.is_some_and(|text| {
            self.program
                .representations
                .value_type(text)
                .and_then(|ty| self.program.representations.repr(ty.repr()))
                == Some(&Repr::ManagedPointer)
        });
        match &instruction.kind {
            InstructionKind::Constant(constant) => {
                let expected = match constant {
                    Constant::Unit => unit,
                    Constant::Bool(_) => boolean,
                    Constant::Int(_) => integer,
                    Constant::FloatBits(_) => float,
                };
                self.require_results(function, instruction, &[expected], &path);
            }
            InstructionKind::TextLiteral { utf8 } => {
                if text.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.result[0]"),
                        "Text literal requires the canonical Text pointer representation",
                    );
                }
                self.require_results(function, instruction, &[text], &path);
                if utf8.len() > crate::TEXT_LITERAL_MAX_BYTES {
                    self.error(
                        ValidationCode::InstructionShape,
                        format!("{path}.utf8"),
                        format!(
                            "Text literal has {} UTF-8 bytes, exceeding the {}-byte per-literal budget",
                            utf8.len(),
                            crate::TEXT_LITERAL_MAX_BYTES
                        ),
                    );
                }
                self.text_literal_bytes = self.text_literal_bytes.saturating_add(utf8.len());
                if self.text_literal_bytes > crate::TEXT_LITERAL_MAX_TOTAL_BYTES {
                    self.error(
                        ValidationCode::InstructionShape,
                        format!("{path}.utf8"),
                        format!(
                            "Text literals exceed the {}-byte artifact budget",
                            crate::TEXT_LITERAL_MAX_TOTAL_BYTES
                        ),
                    );
                }
            }
            InstructionKind::TextConcat { left, right } => {
                if !text_is_managed {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.result[0]"),
                        "Text concatenation requires the canonical managed Text representation",
                    );
                }
                self.require_known_value_type(
                    function,
                    *left,
                    text,
                    ValidationCode::TypeMismatch,
                    format!("{path}.left"),
                );
                self.require_known_value_type(
                    function,
                    *right,
                    text,
                    ValidationCode::TypeMismatch,
                    format!("{path}.right"),
                );
                self.require_results(function, instruction, &[text], &path);
            }
            InstructionKind::TextGet {
                text: value,
                index,
                missing_variant,
                found_variant,
            } => {
                if !text_is_managed {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.result[0]"),
                        "Text selection requires the canonical managed Text representation",
                    );
                }
                self.require_known_value_type(
                    function,
                    *value,
                    text,
                    ValidationCode::TypeMismatch,
                    format!("{path}.text"),
                );
                self.require_known_value_type(
                    function,
                    *index,
                    integer,
                    ValidationCode::TypeMismatch,
                    format!("{path}.index"),
                );
                self.require_results(function, instruction, &[None], &path);

                if missing_variant == found_variant {
                    self.error(
                        ValidationCode::InstructionShape,
                        format!("{path}.variants"),
                        "Text selection requires distinct missing and found variants",
                    );
                }
                if let Some(result) = instruction.results.first().copied()
                    && let Some(result_ty) = function.value(result).map(Value::ty)
                {
                    let semantic_is_option_text = self
                        .program
                        .representations
                        .value_type(result_ty)
                        .is_some_and(|value_type| {
                            matches!(
                                value_type.semantic(),
                                Type::Nominal(identity, arguments)
                                    if Some(*identity) == self.program.canonical_types.option
                                        && arguments.as_slice() == [Type::Text]
                            )
                        });
                    if !semantic_is_option_text {
                        self.error(
                            ValidationCode::TypeMismatch,
                            format!("{path}.result[0]"),
                            "Text selection result must use the cataloged canonical Option[Text] identity",
                        );
                    }
                    let Some(sum) = self.sum_repr(result_ty) else {
                        self.error(
                            ValidationCode::TypeMismatch,
                            format!("{path}.result[0]"),
                            "Text selection result must use a closed sum representation",
                        );
                        return;
                    };
                    let variant_count = self
                        .program
                        .representations
                        .sum(sum)
                        .map_or(0, |sum| sum.variants().len());
                    if variant_count != 2 {
                        self.error(
                            ValidationCode::InstructionShape,
                            format!("{path}.result[0]"),
                            "Text selection Option must contain exactly two variants",
                        );
                    }
                    let missing = usize::try_from(*missing_variant).ok();
                    let found = usize::try_from(*found_variant).ok();
                    if missing.and_then(|variant| self.sum_variant_field_count(sum, variant))
                        != Some(0)
                    {
                        self.error(
                            ValidationCode::InstructionShape,
                            format!("{path}.missing_variant"),
                            "Text selection missing variant must exist and carry no payload",
                        );
                    }
                    let found_shape = found.map(|variant| {
                        (
                            self.sum_variant_field_count(sum, variant),
                            self.sum_variant_field(sum, variant, 0),
                        )
                    });
                    if found_shape != Some((Some(1), text)) {
                        self.error(
                            ValidationCode::InstructionShape,
                            format!("{path}.found_variant"),
                            "Text selection found variant must exist and carry exactly one Text",
                        );
                    }
                }
            }
            InstructionKind::TextLength { text: value } => {
                if text.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.text"),
                        "Text length requires the canonical Text pointer representation",
                    );
                }
                self.require_known_value_type(
                    function,
                    *value,
                    text,
                    ValidationCode::TypeMismatch,
                    format!("{path}.text"),
                );
                self.require_results(function, instruction, &[integer], &path);
            }
            InstructionKind::TextContains {
                text: value,
                needle,
            } => {
                if text.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.text"),
                        "Text containment requires the canonical Text pointer representation",
                    );
                }
                self.require_known_value_type(
                    function,
                    *value,
                    text,
                    ValidationCode::TypeMismatch,
                    format!("{path}.text"),
                );
                self.require_known_value_type(
                    function,
                    *needle,
                    text,
                    ValidationCode::TypeMismatch,
                    format!("{path}.needle"),
                );
                self.require_results(function, instruction, &[boolean], &path);
            }
            InstructionKind::TextCompare { left, right, .. } => {
                if text.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.left"),
                        "Text comparison requires the canonical Text pointer representation",
                    );
                }
                self.require_known_value_type(
                    function,
                    *left,
                    text,
                    ValidationCode::TypeMismatch,
                    format!("{path}.left"),
                );
                self.require_known_value_type(
                    function,
                    *right,
                    text,
                    ValidationCode::TypeMismatch,
                    format!("{path}.right"),
                );
                self.require_results(function, instruction, &[boolean], &path);
            }
            InstructionKind::TextEncodeUtf8 { text: value } => {
                if text.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.text"),
                        "UTF-8 encoding requires the canonical Text pointer representation",
                    );
                }
                if bytes.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.result[0]"),
                        "UTF-8 encoding requires cataloged canonical managed Bytes",
                    );
                }
                self.require_known_value_type(
                    function,
                    *value,
                    text,
                    ValidationCode::TypeMismatch,
                    format!("{path}.text"),
                );
                self.require_results(function, instruction, &[bytes], &path);
            }
            InstructionKind::TextFromUtf8Units {
                units,
                ok_variant,
                error_variant,
                invalid_utf8_variant,
            } => self.validate_text_from_utf8_units_instruction(
                function,
                instruction,
                *units,
                *ok_variant,
                *error_variant,
                *invalid_utf8_variant,
                &path,
            ),
            InstructionKind::ProcessArgumentCount => {
                self.require_results(function, instruction, &[integer], &path);
            }
            InstructionKind::ProcessArgumentAt { index } => {
                if !text_is_managed {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.result[0]"),
                        "process argument selection requires the canonical managed Text representation",
                    );
                }
                self.require_known_value_type(
                    function,
                    *index,
                    integer,
                    ValidationCode::TypeMismatch,
                    format!("{path}.index"),
                );
                self.require_results(function, instruction, &[text], &path);
            }
            InstructionKind::ProcessEnvironment {
                name,
                missing_variant,
                found_variant,
            } => {
                if !text_is_managed {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.result[0]"),
                        "process environment lookup requires the canonical managed Text representation",
                    );
                }
                self.require_known_value_type(
                    function,
                    *name,
                    text,
                    ValidationCode::TypeMismatch,
                    format!("{path}.name"),
                );
                self.require_results(function, instruction, &[None], &path);
                if missing_variant == found_variant {
                    self.error(
                        ValidationCode::InstructionShape,
                        format!("{path}.variants"),
                        "process environment lookup requires distinct missing and found variants",
                    );
                }
                if let Some(result) = instruction.results.first().copied()
                    && let Some(result_ty) = function.value(result).map(Value::ty)
                {
                    let semantic_is_option_text = self
                        .program
                        .representations
                        .value_type(result_ty)
                        .is_some_and(|value_type| {
                            matches!(
                                value_type.semantic(),
                                Type::Nominal(identity, arguments)
                                    if Some(*identity) == self.program.canonical_types.option
                                        && arguments.as_slice() == [Type::Text]
                            )
                        });
                    if !semantic_is_option_text {
                        self.error(
                            ValidationCode::TypeMismatch,
                            format!("{path}.result[0]"),
                            "process environment result must use the cataloged canonical Option[Text] identity",
                        );
                    }
                    let Some(sum) = self.sum_repr(result_ty) else {
                        self.error(
                            ValidationCode::TypeMismatch,
                            format!("{path}.result[0]"),
                            "process environment result must use a closed sum representation",
                        );
                        return;
                    };
                    if self
                        .program
                        .representations
                        .sum(sum)
                        .map_or(0, |sum| sum.variants().len())
                        != 2
                    {
                        self.error(
                            ValidationCode::InstructionShape,
                            format!("{path}.result[0]"),
                            "process environment Option must contain exactly two variants",
                        );
                    }
                    let missing = usize::try_from(*missing_variant).ok();
                    let found = usize::try_from(*found_variant).ok();
                    if missing.and_then(|variant| self.sum_variant_field_count(sum, variant))
                        != Some(0)
                    {
                        self.error(
                            ValidationCode::InstructionShape,
                            format!("{path}.missing_variant"),
                            "process environment missing variant must exist and carry no payload",
                        );
                    }
                    let found_shape = found.map(|variant| {
                        (
                            self.sum_variant_field_count(sum, variant),
                            self.sum_variant_field(sum, variant, 0),
                        )
                    });
                    if found_shape != Some((Some(1), text)) {
                        self.error(
                            ValidationCode::InstructionShape,
                            format!("{path}.found_variant"),
                            "process environment found variant must exist and carry exactly one Text",
                        );
                    }
                }
            }
            InstructionKind::PathFromText {
                text,
                ok_variant,
                error_variant,
                contains_nul_variant,
            } => {
                let canonical = self.canonical_path_type();
                if canonical.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.text"),
                        "Path construction requires cataloged canonical Path as one managed Text field",
                    );
                }
                self.require_known_value_type(
                    function,
                    *text,
                    canonical.map(|(_, text)| text),
                    ValidationCode::TypeMismatch,
                    format!("{path}.text"),
                );
                self.validate_path_result(
                    function,
                    instruction,
                    *ok_variant,
                    *error_variant,
                    *contains_nul_variant,
                    0,
                    &path,
                );
            }
            InstructionKind::PathAsText { path: value } => {
                let canonical = self.canonical_path_type();
                if canonical.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.path"),
                        "Path projection requires cataloged canonical Path as one managed Text field",
                    );
                }
                self.require_known_value_type(
                    function,
                    *value,
                    canonical.map(|(path, _)| path),
                    ValidationCode::TypeMismatch,
                    format!("{path}.path"),
                );
                self.require_results(
                    function,
                    instruction,
                    &[canonical.map(|(_, text)| text)],
                    &path,
                );
            }
            InstructionKind::PathJoin {
                base,
                child,
                ok_variant,
                error_variant,
                absolute_join_variant,
            } => {
                let canonical = self.canonical_path_type();
                if canonical.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.base"),
                        "Path join requires cataloged canonical Path as one managed Text field",
                    );
                }
                for (name, value) in [("base", base), ("child", child)] {
                    self.require_known_value_type(
                        function,
                        *value,
                        canonical.map(|(path, _)| path),
                        ValidationCode::TypeMismatch,
                        format!("{path}.{name}"),
                    );
                }
                self.validate_path_result(
                    function,
                    instruction,
                    *ok_variant,
                    *error_variant,
                    *absolute_join_variant,
                    1,
                    &path,
                );
            }
            InstructionKind::BytesLength { bytes: value } => {
                if bytes.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.bytes"),
                        "Bytes length requires cataloged canonical managed Bytes",
                    );
                }
                self.require_known_value_type(
                    function,
                    *value,
                    bytes,
                    ValidationCode::TypeMismatch,
                    format!("{path}.bytes"),
                );
                self.require_results(function, instruction, &[integer], &path);
            }
            InstructionKind::BytesGet {
                bytes: value,
                index,
                missing_variant,
                found_variant,
            } => {
                if bytes.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.bytes"),
                        "Bytes selection requires cataloged canonical managed Bytes",
                    );
                }
                self.require_known_value_type(
                    function,
                    *value,
                    bytes,
                    ValidationCode::TypeMismatch,
                    format!("{path}.bytes"),
                );
                self.require_known_value_type(
                    function,
                    *index,
                    integer,
                    ValidationCode::TypeMismatch,
                    format!("{path}.index"),
                );
                self.require_results(function, instruction, &[None], &path);
                if (*missing_variant, *found_variant) != (0, 1) {
                    self.error(
                        ValidationCode::InstructionShape,
                        format!("{path}.variants"),
                        "Bytes selection requires canonical None=0 and Some=1 variants",
                    );
                }
                if let Some(result_ty) = instruction
                    .results
                    .first()
                    .and_then(|result| function.value(*result))
                    .map(Value::ty)
                {
                    let option_int = self
                        .program
                        .canonical_types
                        .option
                        .map(|option| Type::Nominal(option, vec![Type::Int]));
                    let exact_semantic = option_int.as_ref().is_some_and(|option_int| {
                        self.program
                            .representations
                            .value_type(result_ty)
                            .is_some_and(|value_type| value_type.semantic() == option_int)
                            && self.scalar_type(option_int) == Some(result_ty)
                    });
                    let exact_shape = self.sum_repr(result_ty).is_some_and(|sum| {
                        self.program.representations.sum(sum).is_some_and(|sum| {
                            sum.variants().len() == 2
                                && sum.variants()[0].fields().is_empty()
                                && integer
                                    .is_some_and(|integer| sum.variants()[1].fields() == [integer])
                        })
                    });
                    if !exact_semantic || !exact_shape {
                        self.error(
                            ValidationCode::TypeMismatch,
                            format!("{path}.result[0]"),
                            "Bytes selection result must be the cataloged canonical Option[Int] with None=0 and Some(Int)=1",
                        );
                    }
                }
            }
            InstructionKind::BytesAppend { left, right } => {
                if bytes.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.result[0]"),
                        "Bytes append requires cataloged canonical managed Bytes",
                    );
                }
                for (name, value) in [("left", left), ("right", right)] {
                    self.require_known_value_type(
                        function,
                        *value,
                        bytes,
                        ValidationCode::TypeMismatch,
                        format!("{path}.{name}"),
                    );
                }
                self.require_results(function, instruction, &[bytes], &path);
            }
            InstructionKind::BytesDecodeUtf8 {
                bytes: value,
                ok_variant,
                error_variant,
                invalid_utf8_variant,
            } => self.validate_bytes_decode_utf8_instruction(
                function,
                instruction,
                *value,
                *ok_variant,
                *error_variant,
                *invalid_utf8_variant,
                &path,
            ),
            InstructionKind::BytesCompare { left, right, .. } => {
                if bytes.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.left"),
                        "Bytes comparison requires cataloged canonical managed Bytes",
                    );
                }
                for (name, value) in [("left", left), ("right", right)] {
                    self.require_known_value_type(
                        function,
                        *value,
                        bytes,
                        ValidationCode::TypeMismatch,
                        format!("{path}.{name}"),
                    );
                }
                self.require_results(function, instruction, &[boolean], &path);
            }
            InstructionKind::FloatParseStatus { text } => {
                self.require_known_value_type(
                    function,
                    *text,
                    self.scalar_type(&Type::Text),
                    ValidationCode::TypeMismatch,
                    format!("{path}.text"),
                );
                let float_status = self.scalar_type(&Type::Tuple(vec![Type::Float, Type::Int]));
                if float_status.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.result"),
                        "float parse status requires the exact (Float, Int) tuple type",
                    );
                }
                self.require_results(function, instruction, &[float_status], &path);
            }
            InstructionKind::FloatFormat { value } => {
                self.require_known_value_type(
                    function,
                    *value,
                    self.scalar_type(&Type::Float),
                    ValidationCode::TypeMismatch,
                    format!("{path}.value"),
                );
                self.require_results(
                    function,
                    instruction,
                    &[self.scalar_type(&Type::Text)],
                    &path,
                );
            }
            InstructionKind::IntToFloat { value } => {
                self.require_known_value_type(
                    function,
                    *value,
                    integer,
                    ValidationCode::TypeMismatch,
                    format!("{path}.value"),
                );
                self.require_results(function, instruction, &[float], &path);
            }
            InstructionKind::FloatToIntStatus { value } => {
                self.require_known_value_type(
                    function,
                    *value,
                    float,
                    ValidationCode::TypeMismatch,
                    format!("{path}.value"),
                );
                self.require_results(function, instruction, &[integer_pair], &path);
            }
            InstructionKind::JsonFormat {
                json,
                ok_variant,
                error_variant,
                depth_limit_variant,
                non_finite_number_variant,
            } => self.validate_json_format_instruction(
                function,
                instruction,
                *json,
                *ok_variant,
                *error_variant,
                *depth_limit_variant,
                *non_finite_number_variant,
                &path,
            ),
            InstructionKind::ProductConstruct { fields }
            | InstructionKind::InvariantRecordProven { fields } => {
                self.require_results(function, instruction, &[None], &path);
                let result_type = instruction
                    .results
                    .first()
                    .and_then(|result| function.value(*result))
                    .map(|result| result.ty);
                let Some(expected_count) = result_type
                    .and_then(|ty| self.product_fields(ty))
                    .map(<[_]>::len)
                else {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.result[0]"),
                        "product construction result must use a product representation",
                    );
                    return;
                };
                if expected_count > crate::repr::DIRECT_PRODUCT_MAX_STRUCTURAL_NODES {
                    self.error(
                        ValidationCode::InstructionShape,
                        format!("{path}.fields"),
                        format!(
                            "product construction exceeds the {}-field validation budget",
                            crate::repr::DIRECT_PRODUCT_MAX_STRUCTURAL_NODES
                        ),
                    );
                    return;
                }
                let result_kind = result_type
                    .and_then(|ty| self.program.representations.value_type(ty))
                    .map(crate::ValueType::kind);
                if matches!(instruction.kind(), InstructionKind::ProductConstruct { .. })
                    && result_type.is_some_and(|ty| self.is_resource_capability_type(ty))
                {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.result[0]"),
                        "File and Socket resource capabilities contain opaque tokens and cannot be constructed with general product instructions",
                    );
                }
                let construction_kind_valid = match &instruction.kind {
                    InstructionKind::ProductConstruct { .. } => {
                        result_kind == Some(ValueTypeKind::Direct)
                    }
                    InstructionKind::InvariantRecordProven { .. } => {
                        result_kind == Some(ValueTypeKind::InvariantProduct)
                    }
                    _ => false,
                };
                if !construction_kind_valid {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.result[0]"),
                        "product construction opcode does not match the result type's checked construction boundary",
                    );
                }
                if fields.len() != expected_count {
                    self.error(
                        ValidationCode::InstructionShape,
                        format!("{path}.fields"),
                        format!(
                            "product construction has {} fields, representation requires {}",
                            fields.len(),
                            expected_count
                        ),
                    );
                }
                for (index, field) in fields.iter().copied().take(expected_count).enumerate() {
                    let expected = result_type
                        .and_then(|ty| self.product_fields(ty))
                        .and_then(|expected| expected.get(index))
                        .copied()
                        .expect("validated product field count remains stable");
                    self.require_value_type(
                        function,
                        field,
                        expected,
                        ValidationCode::TypeMismatch,
                        format!("{path}.field[{index}]"),
                    );
                }
            }
            InstructionKind::ProductExtract { aggregate, field } => {
                let aggregate_type = function.value(*aggregate).map(|value| value.ty);
                if aggregate_type.is_some_and(|ty| self.is_resource_capability_type(ty)) {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.aggregate"),
                        "File and Socket resource capabilities contain opaque tokens and cannot be exposed with general product instructions",
                    );
                }
                if aggregate_type
                    .and_then(|ty| self.program.representations.value_type(ty))
                    .is_some_and(|ty| matches!(ty.kind(), ValueTypeKind::Transparent { .. }))
                {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.aggregate"),
                        "product extraction from a transparent nominal value requires explicit unrefinement",
                    );
                }
                let expected = aggregate_type
                    .and_then(|ty| self.product_fields(ty))
                    .and_then(|fields| {
                        usize::try_from(*field)
                            .ok()
                            .and_then(|index| fields.get(index))
                    })
                    .copied();
                if aggregate_type.is_some_and(|ty| self.product_fields(ty).is_none()) {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.aggregate"),
                        "product extraction operand must use a product representation",
                    );
                } else if aggregate_type.is_some() && expected.is_none() {
                    self.error(
                        ValidationCode::InstructionShape,
                        format!("{path}.field"),
                        format!("product field index {field} is out of range"),
                    );
                }
                self.require_results(function, instruction, &[expected], &path);
                if expected.is_some_and(|field| {
                    representation_contains_task_handle(&self.program.representations, field)
                        == Some(true)
                }) {
                    self.error(
                        ValidationCode::InvalidTaskOwnership,
                        format!("{path}.field"),
                        "product.extract cannot split a Task-bearing field from its affine aggregate owner",
                    );
                }
            }
            InstructionKind::ProductInsert {
                aggregate,
                field,
                value,
            }
            | InstructionKind::InvariantReceiverInsert {
                aggregate,
                field,
                value,
            } => {
                let aggregate_type = function.value(*aggregate).map(|value| value.ty);
                if aggregate_type.is_some_and(|aggregate| {
                    representation_contains_task_handle(&self.program.representations, aggregate)
                        == Some(true)
                }) {
                    self.error(
                        ValidationCode::InvalidTaskOwnership,
                        format!("{path}.aggregate"),
                        "product insertion cannot rebuild or mutate an affine Task-bearing aggregate",
                    );
                }
                if matches!(instruction.kind(), InstructionKind::ProductInsert { .. })
                    && aggregate_type.is_some_and(|ty| self.is_resource_capability_type(ty))
                {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.aggregate"),
                        "File and Socket resource capabilities contain opaque tokens and cannot be replaced with general product instructions",
                    );
                }
                let aggregate_kind = aggregate_type
                    .and_then(|ty| self.program.representations.value_type(ty))
                    .map(crate::ValueType::kind);
                let valid_kind = match &instruction.kind {
                    InstructionKind::ProductInsert { .. } => {
                        aggregate_kind == Some(ValueTypeKind::Direct)
                    }
                    InstructionKind::InvariantReceiverInsert { .. } => {
                        aggregate_kind == Some(ValueTypeKind::InvariantProduct)
                            && aggregate_type.is_some_and(|aggregate_type| {
                                function.signature.inout_params().iter().any(|parameter| {
                                    usize::try_from(*parameter)
                                        .ok()
                                        .and_then(|index| function.signature.params().get(index))
                                        .copied()
                                        == Some(aggregate_type)
                                })
                            })
                    }
                    _ => false,
                };
                if !valid_kind {
                    let message = match &instruction.kind {
                        InstructionKind::ProductInsert { .. } => {
                            "product insertion cannot mutate a transparent or invariant-protected semantic value"
                        }
                        InstructionKind::InvariantReceiverInsert { .. } => {
                            "invariant receiver insertion requires a declared invariant inout receiver"
                        }
                        _ => unreachable!(),
                    };
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.aggregate"),
                        message,
                    );
                }
                let expected_field = aggregate_type
                    .and_then(|ty| self.product_fields(ty))
                    .and_then(|fields| {
                        usize::try_from(*field)
                            .ok()
                            .and_then(|index| fields.get(index))
                    })
                    .copied();
                if aggregate_type.is_some_and(|ty| self.product_fields(ty).is_none()) {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.aggregate"),
                        "product insertion operand must use a product representation",
                    );
                } else if aggregate_type.is_some() && expected_field.is_none() {
                    self.error(
                        ValidationCode::InstructionShape,
                        format!("{path}.field"),
                        format!("product field index {field} is out of range"),
                    );
                }
                self.require_known_value_type(
                    function,
                    *value,
                    expected_field,
                    ValidationCode::TypeMismatch,
                    format!("{path}.value"),
                );
                self.require_results(function, instruction, &[aggregate_type], &path);
            }
            InstructionKind::RefineProven { value } => {
                let operand_type = function.value(*value).map(|value| value.ty);
                let result_type = instruction
                    .results
                    .first()
                    .and_then(|result| function.value(*result))
                    .map(|result| result.ty);
                self.require_results(function, instruction, &[None], &path);
                let expected_base = result_type
                    .and_then(|result| self.program.representations.value_type(result))
                    .and_then(|result| match result.kind() {
                        ValueTypeKind::Transparent { base } => Some(base),
                        ValueTypeKind::Direct
                        | ValueTypeKind::ManagedTextMap
                        | ValueTypeKind::InvariantProduct => None,
                    });
                if result_type.is_some() && expected_base.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.result[0]"),
                        "refine.proven result must be a registered transparent nominal type",
                    );
                }
                self.require_known_value_type(
                    function,
                    *value,
                    expected_base,
                    ValidationCode::TypeMismatch,
                    format!("{path}.value"),
                );
                if expected_base.is_some() && operand_type != expected_base {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.value"),
                        "refine.proven operand must have the transparent type's exact declared base",
                    );
                }
            }
            InstructionKind::Unrefine { value } => {
                let operand_type = function.value(*value).map(|value| value.ty);
                let expected_result = operand_type
                    .and_then(|operand| self.program.representations.value_type(operand))
                    .and_then(|operand| match operand.kind() {
                        ValueTypeKind::Transparent { base } => Some(base),
                        ValueTypeKind::Direct
                        | ValueTypeKind::ManagedTextMap
                        | ValueTypeKind::InvariantProduct => None,
                    });
                if operand_type.is_some() && expected_result.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.value"),
                        "unrefine operand must be a registered transparent nominal type",
                    );
                }
                self.require_results(function, instruction, &[expected_result], &path);
            }
            InstructionKind::SumConstruct { variant, payload } => {
                self.require_results(function, instruction, &[None], &path);
                let result_type = instruction
                    .results
                    .first()
                    .and_then(|result| function.value(*result))
                    .map(|result| result.ty);
                let sum = result_type.and_then(|ty| self.sum_repr(ty));
                if result_type.is_some() && sum.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.result[0]"),
                        "sum construction result must use a sum representation",
                    );
                    return;
                }
                let variant_index = usize::try_from(*variant).ok();
                let expected_count = sum.and_then(|sum| {
                    variant_index.and_then(|index| self.sum_variant_field_count(sum, index))
                });
                let Some(expected_count) = expected_count else {
                    self.error(
                        ValidationCode::InstructionShape,
                        format!("{path}.variant"),
                        format!("sum variant index {variant} is out of range"),
                    );
                    return;
                };
                if payload.len() != expected_count {
                    self.error(
                        ValidationCode::InstructionShape,
                        format!("{path}.payload"),
                        format!(
                            "sum construction has {} payload value(s), variant requires {}",
                            payload.len(),
                            expected_count
                        ),
                    );
                }
                let sum = sum.expect("sum representation was checked above");
                let variant_index = variant_index.expect("variant index was checked above");
                for (index, value) in payload.iter().copied().take(expected_count).enumerate() {
                    let expected = self
                        .sum_variant_field(sum, variant_index, index)
                        .expect("validated sum payload count remains stable");
                    self.require_value_type(
                        function,
                        value,
                        expected,
                        ValidationCode::TypeMismatch,
                        format!("{path}.payload[{index}]"),
                    );
                }
            }
            InstructionKind::DynConstruct { variant, value } => {
                self.require_results(function, instruction, &[None], &path);
                let result_type = instruction
                    .results
                    .first()
                    .and_then(|result| function.value(*result))
                    .map(|result| result.ty);
                let candidates = result_type
                    .and_then(|ty| self.program.representations.dynamic(ty))
                    .map(crate::DynamicRepr::candidates);
                if result_type.is_some() && candidates.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.result[0]"),
                        "dynamic construction result must use a cataloged managed View representation",
                    );
                }
                let expected = usize::try_from(*variant)
                    .ok()
                    .and_then(|index| candidates.and_then(|values| values.get(index)))
                    .copied();
                if candidates.is_some() && expected.is_none() {
                    self.error(
                        ValidationCode::InstructionShape,
                        format!("{path}.variant"),
                        format!("dynamic candidate index {variant} is out of range"),
                    );
                }
                self.require_known_value_type(
                    function,
                    *value,
                    expected,
                    ValidationCode::TypeMismatch,
                    format!("{path}.value"),
                );
            }
            InstructionKind::ListConstruct { elements } => {
                self.require_results(function, instruction, &[None], &path);
                let result_type = instruction
                    .results
                    .first()
                    .and_then(|result| function.value(*result))
                    .map(|result| result.ty);
                let element_type = result_type.and_then(|ty| self.list_element(ty));
                if result_type.is_some() && element_type.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.result[0]"),
                        "list construction result must be a canonical concrete List value",
                    );
                }
                if elements.len() > crate::LIST_LITERAL_MAX_ELEMENTS {
                    self.error(
                        ValidationCode::InstructionShape,
                        format!("{path}.elements"),
                        format!(
                            "list construction has {} elements, exceeding the {}-element budget",
                            elements.len(),
                            crate::LIST_LITERAL_MAX_ELEMENTS
                        ),
                    );
                }
                for (index, element) in elements
                    .iter()
                    .copied()
                    .take(crate::LIST_LITERAL_MAX_ELEMENTS)
                    .enumerate()
                {
                    self.require_known_value_type(
                        function,
                        element,
                        element_type,
                        ValidationCode::TypeMismatch,
                        format!("{path}.element[{index}]"),
                    );
                }
            }
            InstructionKind::ListAppend { list, value }
            | InstructionKind::ListAppendUnique { list, value } => {
                let list_type = function.value(*list).map(|value| value.ty);
                let element_type = list_type.and_then(|ty| self.list_element(ty));
                if list_type.is_some() && element_type.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.list"),
                        "list append receiver must be a canonical concrete List value",
                    );
                }
                self.require_known_value_type(
                    function,
                    *value,
                    element_type,
                    ValidationCode::TypeMismatch,
                    format!("{path}.value"),
                );
                self.require_results(function, instruction, &[list_type], &path);
            }
            InstructionKind::ListLength { list } => {
                let list_type = function.value(*list).map(|value| value.ty);
                if list_type.is_some_and(|ty| self.list_element(ty).is_none()) {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.list"),
                        "list length receiver must be a canonical concrete List value",
                    );
                }
                self.require_results(function, instruction, &[integer], &path);
            }
            InstructionKind::ListGet { list, index } => {
                let list_element = function
                    .value(*list)
                    .map(|value| value.ty)
                    .and_then(|ty| self.list_element(ty));
                if function.value(*list).is_some() && list_element.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.list"),
                        "list get receiver must be a canonical concrete List value",
                    );
                }
                if list_element.is_some_and(|element| {
                    self.program
                        .representations
                        .value_type(element)
                        .is_some_and(|element| {
                            self.program.representations.repr(element.repr())
                                == Some(&Repr::TaskHandle)
                        })
                }) {
                    self.error(
                        ValidationCode::InvalidTaskOwnership,
                        format!("{path}.list"),
                        "list.get cannot extract or duplicate a child handle from a List[Task[T]] carrier",
                    );
                }
                self.require_known_value_type(
                    function,
                    *index,
                    integer,
                    ValidationCode::TypeMismatch,
                    format!("{path}.index"),
                );
                self.require_results(function, instruction, &[None], &path);
                let result_type = instruction
                    .results
                    .first()
                    .and_then(|result| function.value(*result))
                    .map(|result| result.ty);
                let option_element = result_type.and_then(|ty| self.option_element(ty));
                if result_type.is_some() && option_element.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.result[0]"),
                        "list get result must be the canonical two-variant Option[element] sum shape",
                    );
                } else if list_element.is_some() && option_element != list_element {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.result[0]"),
                        "list get Option payload must exactly match the receiver element type",
                    );
                }
            }
            InstructionKind::TextMapConstruct => {
                self.require_results(function, instruction, &[None], &path);
                let result_type = instruction
                    .results
                    .first()
                    .and_then(|result| function.value(*result))
                    .map(|result| result.ty);
                if result_type.is_some_and(|ty| self.text_map_value(ty).is_none()) {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.result[0]"),
                        "TextMap construction result must be a canonical closed TextMap value",
                    );
                }
            }
            InstructionKind::TextMapConstructEntries { entries } => {
                let entries_type = function.value(*entries).map(|value| value.ty);
                let result_type = entries_type.and_then(|ty| self.text_map_bulk_result(ty));
                if entries_type.is_some() && result_type.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.entries"),
                        "TextMap bulk construction requires canonical List[(Text, V)] and Result[TextMap[V], Text] representations",
                    );
                }
                self.require_results(function, instruction, &[result_type], &path);
            }
            InstructionKind::TextMapInsert { map, key, value } => {
                let map_type = function.value(*map).map(|value| value.ty);
                let value_type = map_type.and_then(|ty| self.text_map_value(ty));
                if map_type.is_some() && value_type.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.map"),
                        "TextMap insert receiver must be a canonical closed TextMap value",
                    );
                }
                self.require_known_value_type(
                    function,
                    *key,
                    text,
                    ValidationCode::TypeMismatch,
                    format!("{path}.key"),
                );
                self.require_known_value_type(
                    function,
                    *value,
                    value_type,
                    ValidationCode::TypeMismatch,
                    format!("{path}.value"),
                );
                self.require_results(function, instruction, &[map_type], &path);
            }
            InstructionKind::TextMapLength { map } => {
                let map_type = function.value(*map).map(|value| value.ty);
                if map_type.is_some_and(|ty| self.text_map_value(ty).is_none()) {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.map"),
                        "TextMap length receiver must be a canonical closed TextMap value",
                    );
                }
                self.require_results(function, instruction, &[integer], &path);
            }
            InstructionKind::TextMapContains { map, key } => {
                let map_type = function.value(*map).map(|value| value.ty);
                if map_type.is_some_and(|ty| self.text_map_value(ty).is_none()) {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.map"),
                        "TextMap contains receiver must be a canonical closed TextMap value",
                    );
                }
                self.require_known_value_type(
                    function,
                    *key,
                    text,
                    ValidationCode::TypeMismatch,
                    format!("{path}.key"),
                );
                self.require_results(function, instruction, &[boolean], &path);
            }
            InstructionKind::TextMapGet { map, key } => {
                let map_value = function
                    .value(*map)
                    .map(|value| value.ty)
                    .and_then(|ty| self.text_map_value(ty));
                if function.value(*map).is_some() && map_value.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.map"),
                        "TextMap get receiver must be a canonical closed TextMap value",
                    );
                }
                self.require_known_value_type(
                    function,
                    *key,
                    text,
                    ValidationCode::TypeMismatch,
                    format!("{path}.key"),
                );
                self.require_results(function, instruction, &[None], &path);
                let result_type = instruction
                    .results
                    .first()
                    .and_then(|result| function.value(*result))
                    .map(|result| result.ty);
                let option_value = result_type.and_then(|ty| self.option_element(ty));
                if result_type.is_some() && option_value.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.result[0]"),
                        "TextMap get result must be the canonical two-variant Option[value] sum shape",
                    );
                } else if map_value.is_some() && option_value != map_value {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.result[0]"),
                        "TextMap get Option payload must exactly match the receiver value type",
                    );
                }
            }
            InstructionKind::TextMapRemove { map, key } => {
                let map_type = function.value(*map).map(|value| value.ty);
                if map_type.is_some_and(|ty| self.text_map_value(ty).is_none()) {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.map"),
                        "TextMap remove receiver must be a canonical closed TextMap value",
                    );
                }
                self.require_known_value_type(
                    function,
                    *key,
                    text,
                    ValidationCode::TypeMismatch,
                    format!("{path}.key"),
                );
                self.require_results(function, instruction, &[map_type], &path);
            }
            InstructionKind::TextMapEntryGet { map, index } => {
                let map_type = function.value(*map).map(|value| value.ty);
                let entry_type = map_type.and_then(|ty| self.text_map_entry(ty));
                if map_type.is_some() && entry_type.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.map"),
                        "TextMap entry read receiver must be a canonical closed TextMap value with an exact (Text, V) entry product",
                    );
                }
                self.require_known_value_type(
                    function,
                    *index,
                    integer,
                    ValidationCode::TypeMismatch,
                    format!("{path}.index"),
                );
                self.require_results(function, instruction, &[None], &path);
                let result_type = instruction
                    .results
                    .first()
                    .and_then(|result| function.value(*result))
                    .map(|result| result.ty);
                let option_entry = result_type.and_then(|ty| self.option_element(ty));
                if result_type.is_some() && option_entry.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.result[0]"),
                        "TextMap entry read result must be the canonical Option[(Text, V)] sum shape",
                    );
                } else if entry_type.is_some() && option_entry != entry_type {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.result[0]"),
                        "TextMap entry read Option payload must exactly match the receiver (Text, V) entry type",
                    );
                }
            }
            InstructionKind::BoolNot { value } => {
                self.require_known_value_type(
                    function,
                    *value,
                    boolean,
                    ValidationCode::TypeMismatch,
                    format!("{path}.operand[0]"),
                );
                self.require_results(function, instruction, &[boolean], &path);
            }
            InstructionKind::BoolCompare { left, right, .. } => {
                self.require_known_value_type(
                    function,
                    *left,
                    boolean,
                    ValidationCode::TypeMismatch,
                    format!("{path}.operand[0]"),
                );
                self.require_known_value_type(
                    function,
                    *right,
                    boolean,
                    ValidationCode::TypeMismatch,
                    format!("{path}.operand[1]"),
                );
                self.require_results(function, instruction, &[boolean], &path);
            }
            InstructionKind::FloatNegate { value } => {
                self.require_known_value_type(
                    function,
                    *value,
                    float,
                    ValidationCode::TypeMismatch,
                    format!("{path}.operand[0]"),
                );
                self.require_results(function, instruction, &[float], &path);
            }
            InstructionKind::FloatBinary { left, right, .. } => {
                self.require_known_value_type(
                    function,
                    *left,
                    float,
                    ValidationCode::TypeMismatch,
                    format!("{path}.operand[0]"),
                );
                self.require_known_value_type(
                    function,
                    *right,
                    float,
                    ValidationCode::TypeMismatch,
                    format!("{path}.operand[1]"),
                );
                self.require_results(function, instruction, &[float], &path);
            }
            InstructionKind::IntCompare { left, right, .. } => {
                self.require_known_value_type(
                    function,
                    *left,
                    integer,
                    ValidationCode::TypeMismatch,
                    format!("{path}.operand[0]"),
                );
                self.require_known_value_type(
                    function,
                    *right,
                    integer,
                    ValidationCode::TypeMismatch,
                    format!("{path}.operand[1]"),
                );
                self.require_results(function, instruction, &[boolean], &path);
            }
            InstructionKind::IntSuccessorBelow {
                value,
                upper_bound,
                proof,
            } => {
                self.require_known_value_type(
                    function,
                    *value,
                    integer,
                    ValidationCode::TypeMismatch,
                    format!("{path}.operand[0]"),
                );
                self.require_known_value_type(
                    function,
                    *upper_bound,
                    integer,
                    ValidationCode::TypeMismatch,
                    format!("{path}.operand[1]"),
                );
                self.require_known_value_type(
                    function,
                    *proof,
                    boolean,
                    ValidationCode::TypeMismatch,
                    format!("{path}.operand[2]"),
                );
                self.require_results(function, instruction, &[integer], &path);
            }
            InstructionKind::FloatCompare { left, right, .. } => {
                self.require_known_value_type(
                    function,
                    *left,
                    float,
                    ValidationCode::TypeMismatch,
                    format!("{path}.operand[0]"),
                );
                self.require_known_value_type(
                    function,
                    *right,
                    float,
                    ValidationCode::TypeMismatch,
                    format!("{path}.operand[1]"),
                );
                self.require_results(function, instruction, &[boolean], &path);
            }
            InstructionKind::TaskCreate {
                coroutine,
                arguments,
            } => {
                let coroutine_id = *coroutine;
                let Some(callee) = self.program.function(coroutine_id) else {
                    self.error(
                        ValidationCode::InvalidFunctionReference,
                        format!("{path}.coroutine"),
                        format!("coroutine {coroutine_id} does not exist"),
                    );
                    self.require_results(function, instruction, &[None], &path);
                    return;
                };
                if callee.coroutine().is_none() {
                    self.error(
                        ValidationCode::CallShape,
                        format!("{path}.coroutine"),
                        "task.create requires a checked coroutine instance",
                    );
                }
                if callee
                    .coroutine()
                    .is_some_and(crate::CoroutinePlan::carries_caller_span)
                {
                    self.validate_contract_fault_span(
                        instruction.origin().span,
                        &format!("{path}.origin.span"),
                    );
                }
                if !callee.signature().inout_params().is_empty() {
                    self.error(
                        ValidationCode::CallShape,
                        format!("{path}.coroutine"),
                        "task.create does not admit an inout coroutine signature",
                    );
                }
                self.validate_call_arguments(
                    function,
                    arguments,
                    callee,
                    &format!("{path}.argument"),
                );
                self.require_results(function, instruction, &[None], &path);
                let result_type = instruction
                    .results()
                    .first()
                    .and_then(|result| function.value(*result))
                    .map(crate::Value::ty);
                let result =
                    result_type.and_then(|result| self.program.representations.value_type(result));
                let coroutine_output = callee.signature().result();
                let canonical_output = self
                    .program
                    .representations
                    .value_type(coroutine_output)
                    .and_then(|output| self.program.representations.type_id(output.semantic()));
                if self
                    .program
                    .representations
                    .value_type(coroutine_output)
                    .is_some()
                    && canonical_output != Some(coroutine_output)
                {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.coroutine.result"),
                        "task.create coroutine output must use its canonical value representation",
                    );
                }
                let valid_result = result_type.zip(result).is_some_and(|(result_type, result)| {
                    self.program.representations.repr(result.repr()) == Some(&Repr::TaskHandle)
                        && self.program.representations.type_id(result.semantic())
                            == Some(result_type)
                        && canonical_output == Some(coroutine_output)
                        && matches!(
                            result.semantic(),
                            Type::Task(output)
                                if self.program
                                    .representations
                                    .value_type(callee.signature().result())
                                    .is_some_and(|expected| expected.semantic() == output.as_ref())
                        )
                });
                if result.is_some() && !valid_result {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.result[0]"),
                        "task.create result must be the canonical Task[coroutine output] handle",
                    );
                }
                if !function.effects().contains(Effects::NEEDS_EXECUTOR) {
                    self.error(
                        ValidationCode::EffectMismatch,
                        &path,
                        "task.create requires the function's NEEDS_EXECUTOR effect",
                    );
                }
            }
            InstructionKind::IoTaskCreate {
                operation,
                error_mode,
                arguments,
            } => {
                let file = self.canonical_resource_type(crate::ResourceKind::File);
                let socket = self.canonical_resource_type(crate::ResourceKind::Socket);
                let text = self.scalar_type(&Type::Text).filter(|ty| {
                    self.program
                        .representations
                        .value_type(*ty)
                        .and_then(|value_type| self.program.representations.repr(value_type.repr()))
                        == Some(&Repr::ManagedPointer)
                });
                let integer = self.scalar_type(&Type::Int);
                let path_type = self.canonical_path_type().map(|(path, _)| path);
                if text.is_none() {
                    self.error(
                        ValidationCode::InstructionShape,
                        &path,
                        "typed I/O requires the canonical managed Text representation",
                    );
                }
                if matches!(
                    operation,
                    IoTaskOperation::FileOpenRead
                        | IoTaskOperation::FileCreate
                        | IoTaskOperation::FileReadText
                        | IoTaskOperation::FileWriteText
                ) && file.is_none()
                {
                    self.error(
                        ValidationCode::InstructionShape,
                        &path,
                        "typed file I/O requires the canonical one-Int File representation",
                    );
                }
                if matches!(
                    operation,
                    IoTaskOperation::SocketConnect
                        | IoTaskOperation::SocketReadText
                        | IoTaskOperation::SocketWriteText
                ) && socket.is_none()
                {
                    self.error(
                        ValidationCode::InstructionShape,
                        &path,
                        "typed socket I/O requires the canonical one-Int Socket representation",
                    );
                }
                if matches!(operation, IoTaskOperation::SocketConnect) && integer.is_none() {
                    self.error(
                        ValidationCode::InstructionShape,
                        &path,
                        "typed socket connect requires the canonical Int representation",
                    );
                }
                let (expected_arguments, success) = match operation {
                    IoTaskOperation::FileOpenRead | IoTaskOperation::FileCreate => {
                        if arguments.len() != 1 {
                            self.error(
                                ValidationCode::InstructionShape,
                                format!("{path}.arguments"),
                                "file open/create requires exactly one Text or Path argument",
                            );
                        }
                        if let Some(argument) = arguments.first().copied() {
                            let actual = function.value(argument).map(Value::ty);
                            if actual != text && actual != path_type {
                                self.error(
                                    ValidationCode::TypeMismatch,
                                    format!("{path}.argument[0]"),
                                    "file open/create argument must be canonical managed Text or Path",
                                );
                            }
                        }
                        (Vec::new(), file)
                    }
                    IoTaskOperation::FileReadText => (vec![file], text),
                    IoTaskOperation::FileWriteText => {
                        (vec![file, text], self.scalar_type(&Type::Unit))
                    }
                    IoTaskOperation::SocketConnect => (vec![text, integer], socket),
                    IoTaskOperation::SocketReadText => (vec![socket], text),
                    IoTaskOperation::SocketWriteText => {
                        (vec![socket, text], self.scalar_type(&Type::Unit))
                    }
                };
                if !expected_arguments.is_empty() {
                    if arguments.len() != expected_arguments.len() {
                        self.error(
                            ValidationCode::InstructionShape,
                            format!("{path}.arguments"),
                            format!(
                                "typed I/O operation requires {} arguments, got {}",
                                expected_arguments.len(),
                                arguments.len()
                            ),
                        );
                    }
                    for (index, (argument, expected)) in arguments
                        .iter()
                        .copied()
                        .zip(expected_arguments)
                        .enumerate()
                    {
                        self.require_known_value_type(
                            function,
                            argument,
                            expected,
                            ValidationCode::TypeMismatch,
                            format!("{path}.argument[{index}]"),
                        );
                    }
                }
                let task = match error_mode {
                    IoTaskErrorMode::Result => {
                        let io_error = self.canonical_io_error_type();
                        if io_error.is_none() {
                            self.error(
                                ValidationCode::InstructionShape,
                                &path,
                                "Result-mode typed I/O requires the canonical IoError and IoErrorKind representations",
                            );
                        }
                        let result = success
                            .zip(io_error)
                            .zip(self.program.canonical_types.result)
                            .and_then(|((success, io_error), result)| {
                                let success = self
                                    .program
                                    .representations
                                    .value_type(success)?
                                    .semantic()
                                    .clone();
                                let error = self
                                    .program
                                    .representations
                                    .value_type(io_error)?
                                    .semantic()
                                    .clone();
                                self.program
                                    .representations
                                    .type_id(&Type::Nominal(result, vec![success, error]))
                            });
                        if success.is_some() && io_error.is_some() && result.is_none() {
                            self.error(
                                ValidationCode::InstructionShape,
                                format!("{path}.result[0]"),
                                "Result-mode typed I/O requires an exact canonical Result[success, IoError] registration",
                            );
                        }
                        if result.is_some_and(|result| {
                            !self.canonical_io_result_shape(result, success, io_error)
                        }) {
                            self.error(
                                ValidationCode::InstructionShape,
                                format!("{path}.result[0]"),
                                "Result-mode typed I/O requires canonical Result[success, IoError] variants",
                            );
                        }
                        let registered_task = result.and_then(|result| {
                            let result = self
                                .program
                                .representations
                                .value_type(result)?
                                .semantic()
                                .clone();
                            self.program
                                .representations
                                .type_id(&Type::Task(Box::new(result)))
                        });
                        let task = registered_task.filter(|task| {
                            result.is_some_and(|result| self.canonical_task_handle(*task, result))
                        });
                        if result.is_some() && task.is_none() {
                            self.error(
                                ValidationCode::InstructionShape,
                                format!("{path}.result[0]"),
                                "Result-mode typed I/O requires an exact canonical Task[Result[success, IoError]] handle registration",
                            );
                        }
                        task
                    }
                    IoTaskErrorMode::Fault => {
                        let registered_task = success.and_then(|success| {
                            let output = self
                                .program
                                .representations
                                .value_type(success)?
                                .semantic()
                                .clone();
                            self.program
                                .representations
                                .type_id(&Type::Task(Box::new(output)))
                        });
                        let task = registered_task.filter(|task| {
                            success
                                .is_some_and(|success| self.canonical_task_handle(*task, success))
                        });
                        if success.is_some() && task.is_none() {
                            self.error(
                                ValidationCode::InstructionShape,
                                format!("{path}.result[0]"),
                                "Fault-mode typed I/O requires an exact canonical Task[success] handle registration",
                            );
                        }
                        task
                    }
                };
                self.require_results(function, instruction, &[task], &path);
                if !function.effects().contains(Effects::NEEDS_EXECUTOR) {
                    self.error(
                        ValidationCode::EffectMismatch,
                        &path,
                        "typed I/O Task creation requires the function's NEEDS_EXECUTOR effect",
                    );
                }
            }
            InstructionKind::ResourceClose { kind, resource } => {
                let resource_type = function.value(*resource).map(super::ir::Value::ty);
                let valid_resource = resource_type == self.canonical_resource_type(*kind);
                if resource_type.is_some() && !valid_resource {
                    let expected = match kind {
                        crate::ResourceKind::File => "File",
                        crate::ResourceKind::Socket => "Socket",
                    };
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.resource"),
                        format!(
                            "typed resource close requires canonical {expected} as one direct canonical Int capability token"
                        ),
                    );
                }
                self.require_results(
                    function,
                    instruction,
                    &[self.scalar_type(&Type::Unit), resource_type],
                    &path,
                );
                if !function.effects.contains(Effects::NEEDS_EXECUTOR) {
                    self.error(
                        ValidationCode::EffectMismatch,
                        &path,
                        "typed resource close requires the function's NEEDS_EXECUTOR effect",
                    );
                }
            }
            InstructionKind::TaskJoin { mode, tasks } => {
                if tasks.is_empty() {
                    self.error(
                        ValidationCode::InstructionShape,
                        format!("{path}.tasks"),
                        "task.join requires at least one child Task",
                    );
                }
                if tasks.iter().copied().collect::<BTreeSet<_>>().len() != tasks.len() {
                    self.error(
                        ValidationCode::InstructionShape,
                        format!("{path}.tasks"),
                        "task.join cannot consume the same child Task more than once",
                    );
                }
                let mut output_types = Vec::with_capacity(tasks.len());
                let mut output_semantics = Vec::with_capacity(tasks.len());
                let mut valid = !tasks.is_empty();
                for (index, task) in tasks.iter().copied().enumerate() {
                    let output = self.task_output_type(function, task);
                    if function.value(task).is_some() && output.is_none() {
                        self.error(
                            ValidationCode::TypeMismatch,
                            format!("{path}.task[{index}]"),
                            "task.join operand must be a canonical concrete Task handle",
                        );
                    }
                    let Some(output) = output else {
                        valid = false;
                        continue;
                    };
                    let Some(semantic) = self
                        .program
                        .representations
                        .value_type(output)
                        .map(crate::ValueType::semantic)
                        .cloned()
                    else {
                        valid = false;
                        continue;
                    };
                    output_types.push(output);
                    output_semantics.push(semantic);
                }
                if valid
                    && matches!(mode, AwaitMode::Any | AwaitMode::Race)
                    && output_types
                        .first()
                        .is_some_and(|first| output_types.iter().any(|output| output != first))
                {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.tasks"),
                        format!(
                            "task.join {} requires every child Task to have the same output type",
                            match mode {
                                AwaitMode::Any => "any",
                                AwaitMode::Race => "race",
                                AwaitMode::All | AwaitMode::Settled => unreachable!(),
                            }
                        ),
                    );
                    valid = false;
                }
                let joined_output = valid.then(|| match mode {
                    AwaitMode::All => self
                        .program
                        .representations
                        .type_id(&Type::Tuple(output_semantics)),
                    AwaitMode::Any => output_types.first().copied(),
                    AwaitMode::Settled => output_types
                        .iter()
                        .copied()
                        .map(|output| self.canonical_task_outcome_type(output))
                        .collect::<Option<Vec<_>>>()
                        .and_then(|outcomes| {
                            outcomes
                                .iter()
                                .map(|outcome| {
                                    self.program
                                        .representations
                                        .value_type(*outcome)
                                        .map(crate::ValueType::semantic)
                                        .cloned()
                                })
                                .collect::<Option<Vec<_>>>()
                        })
                        .and_then(|outcomes| {
                            self.program.representations.type_id(&Type::Tuple(outcomes))
                        }),
                    AwaitMode::Race => output_types
                        .first()
                        .copied()
                        .and_then(|output| self.canonical_task_outcome_type(output)),
                });
                let expected = joined_output.flatten().and_then(|output| {
                    self.program
                        .representations
                        .value_type(output)
                        .map(crate::ValueType::semantic)
                        .cloned()
                        .and_then(|output| {
                            self.program
                                .representations
                                .type_id(&Type::Task(Box::new(output)))
                        })
                });
                if valid && expected.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.result[0]"),
                        "task.join requires the canonical mode-specific Task result",
                    );
                }
                self.require_results(function, instruction, &[expected], &path);
                if !function.effects().contains(Effects::NEEDS_EXECUTOR) {
                    self.error(
                        ValidationCode::EffectMismatch,
                        &path,
                        "task.join requires the function's NEEDS_EXECUTOR effect",
                    );
                }
            }
            InstructionKind::TaskJoinList { mode, tasks } => {
                let list_type = function.value(*tasks).map(Value::ty);
                let output = list_type.and_then(|list| self.task_list_output(list));
                if list_type.is_some() && output.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.tasks"),
                        "task.join_list operand must be one canonical exact List[Task[T]] carrier",
                    );
                }
                let joined_output = output.and_then(|output| match mode {
                    AwaitMode::All => self
                        .program
                        .representations
                        .value_type(output)
                        .map(crate::ValueType::semantic)
                        .cloned()
                        .and_then(|output| {
                            self.program
                                .representations
                                .type_id(&Type::List(Box::new(output)))
                        }),
                    AwaitMode::Any => Some(output),
                    AwaitMode::Settled => self
                        .canonical_task_outcome_type(output)
                        .and_then(|outcome| {
                            self.program
                                .representations
                                .value_type(outcome)
                                .map(crate::ValueType::semantic)
                                .cloned()
                        })
                        .and_then(|outcome| {
                            self.program
                                .representations
                                .type_id(&Type::List(Box::new(outcome)))
                        }),
                    AwaitMode::Race => self.canonical_task_outcome_type(output),
                });
                let expected = joined_output.and_then(|output| {
                    self.program
                        .representations
                        .value_type(output)
                        .map(crate::ValueType::semantic)
                        .cloned()
                        .and_then(|output| {
                            self.program
                                .representations
                                .type_id(&Type::Task(Box::new(output)))
                        })
                });
                if output.is_some() && expected.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.result[0]"),
                        "task.join_list requires the canonical mode-specific Task result",
                    );
                }
                self.require_results(function, instruction, &[expected], &path);
                if !function.effects().contains(Effects::NEEDS_EXECUTOR) {
                    self.error(
                        ValidationCode::EffectMismatch,
                        &path,
                        "task.join_list requires the function's NEEDS_EXECUTOR effect",
                    );
                }
            }
            InstructionKind::TaskOutcomeTake { task } => {
                self.validate_task_outcome_take(function, instruction, *task, &path);
            }
            InstructionKind::DirectCall { callee, arguments } => {
                let callee_id = *callee;
                let Some(callee) = self.program.function(callee_id) else {
                    self.error(
                        ValidationCode::InvalidFunctionReference,
                        format!("{path}.callee"),
                        format!("callee {callee_id} does not exist"),
                    );
                    return;
                };
                if callee.coroutine().is_some() {
                    self.error(
                        ValidationCode::CallShape,
                        format!("{path}.callee"),
                        "direct call cannot target a coroutine constructor; use task.create",
                    );
                }
                if let Some(effects) = self.exact_effect(callee_id) {
                    if effects.contains(Effects::MAY_FAULT) {
                        self.error(
                            ValidationCode::CallShape,
                            format!("{path}.callee"),
                            "direct call requires an infallible callee; use invoke for a may-fault callee",
                        );
                    }
                    if effects.contains(Effects::MAY_SUSPEND) {
                        self.error(
                            ValidationCode::CallShape,
                            format!("{path}.callee"),
                            "direct call cannot target a suspending callee",
                        );
                    }
                }
                if arguments.len() != callee.signature.params().len() {
                    self.error(
                        ValidationCode::CallShape,
                        format!("{path}.arguments"),
                        format!(
                            "call has {} arguments, callee requires {}",
                            arguments.len(),
                            callee.signature.params().len()
                        ),
                    );
                }
                for (index, (argument, expected)) in arguments
                    .iter()
                    .copied()
                    .zip(callee.signature.params().iter().copied())
                    .enumerate()
                {
                    self.require_value_type(
                        function,
                        argument,
                        expected,
                        ValidationCode::CallShape,
                        format!("{path}.argument[{index}]"),
                    );
                }
                let mut results = Vec::with_capacity(1 + callee.signature.inout_params().len());
                results.push(Some(callee.signature.result()));
                results.extend(
                    Self::signature_writeback_types(callee)
                        .into_iter()
                        .map(Some),
                );
                self.require_results(function, instruction, &results, &path);
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn validate_terminator(
        &mut self,
        function: &Function,
        block_index: usize,
        terminator: &Terminator,
        base: &str,
    ) {
        let path = format!("{base}.block[{block_index}].terminator");
        if terminator.origin.source_function != function.source() {
            self.error(
                ValidationCode::OriginMismatch,
                format!("{path}.origin"),
                format!(
                    "origin names source function f{}, expected f{}",
                    terminator.origin.source_function.0,
                    function.source().0
                ),
            );
        }
        // A conditional branch may select two distinct argument lists for the
        // same destination. Backends normalize those logical edges to distinct
        // physical predecessor blocks before constructing phis. Result and
        // unwind edges still have incompatible value/fault-state semantics and
        // must remain unique.
        if !matches!(terminator.kind(), TerminatorKind::Branch { .. }) {
            let mut targets = BTreeSet::new();
            for edge in terminator.control_flow_edges() {
                if !targets.insert(edge.block) {
                    self.error(
                        ValidationCode::DuplicateSuccessor,
                        path.clone(),
                        format!(
                            "terminator has multiple edges from one block to {}; split the edges",
                            edge.block
                        ),
                    );
                }
            }
        }
        let terminal_exit = matches!(
            terminator.kind(),
            TerminatorKind::Return(_)
                | TerminatorKind::Fault { .. }
                | TerminatorKind::ResumeFault
                | TerminatorKind::TaskCancelled
        );
        if terminal_exit {
            let expected = Self::signature_writeback_types(function);
            if terminator.writebacks().len() != expected.len() {
                self.error(
                    ValidationCode::InOutShape,
                    format!("{path}.writebacks"),
                    format!(
                        "terminal exit carries {} writebacks, signature requires {}",
                        terminator.writebacks().len(),
                        expected.len()
                    ),
                );
            }
            for (index, (value, expected)) in terminator
                .writebacks()
                .iter()
                .copied()
                .zip(expected)
                .enumerate()
            {
                self.require_value_type(
                    function,
                    value,
                    expected,
                    ValidationCode::InOutShape,
                    format!("{path}.writeback[{index}]"),
                );
            }
        } else if !terminator.writebacks().is_empty() {
            self.error(
                ValidationCode::InOutShape,
                format!("{path}.writebacks"),
                "only return, fault, and resume_fault may carry function writebacks",
            );
        }
        match terminator.kind() {
            TerminatorKind::Jump(target) => {
                self.validate_target(function, target, format!("{path}.target"));
            }
            TerminatorKind::Branch {
                condition,
                then_target,
                else_target,
            } => {
                self.require_known_value_type(
                    function,
                    *condition,
                    self.scalar_type(&Type::Bool),
                    ValidationCode::TypeMismatch,
                    format!("{path}.condition"),
                );
                self.validate_target(function, then_target, format!("{path}.then"));
                self.validate_target(function, else_target, format!("{path}.else"));
            }
            TerminatorKind::SumSwitch { scrutinee, cases } => {
                let scrutinee_type = function.value(*scrutinee).map(|value| value.ty);
                let sum = scrutinee_type.and_then(|ty| self.sum_repr(ty));
                if scrutinee_type.is_some() && sum.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.scrutinee"),
                        "sum switch scrutinee must use a sum representation",
                    );
                }
                let Some(sum) = sum else {
                    return;
                };
                let variants = self
                    .program
                    .representations
                    .sum(sum)
                    .map(|sum| sum.variants().len())
                    .unwrap_or_default();
                if cases.len() != variants {
                    self.error(
                        ValidationCode::InstructionShape,
                        format!("{path}.cases"),
                        format!(
                            "sum switch has {} case(s), representation requires {}",
                            cases.len(),
                            variants
                        ),
                    );
                }
                for (index, case) in cases.iter().take(variants).enumerate() {
                    if usize::try_from(case.variant).ok() != Some(index) {
                        self.error(
                            ValidationCode::InstructionShape,
                            format!("{path}.case[{index}].variant"),
                            format!(
                                "sum switch cases must be ordered 0..n, found variant {}",
                                case.variant
                            ),
                        );
                    }
                    self.validate_sum_case(
                        function,
                        case,
                        sum,
                        index,
                        &format!("{path}.case[{index}]"),
                    );
                }
            }
            TerminatorKind::DynSwitch { scrutinee, cases } => {
                let scrutinee_type = function.value(*scrutinee).map(|value| value.ty);
                let candidates = scrutinee_type
                    .and_then(|ty| self.program.representations.dynamic(ty))
                    .map(crate::DynamicRepr::candidates);
                if scrutinee_type.is_some() && candidates.is_none() {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.scrutinee"),
                        "dynamic switch scrutinee must use a cataloged managed View representation",
                    );
                }
                let Some(candidates) = candidates else {
                    return;
                };
                if cases.len() != candidates.len() {
                    self.error(
                        ValidationCode::InstructionShape,
                        format!("{path}.cases"),
                        format!(
                            "dynamic switch has {} case(s), catalog requires {}",
                            cases.len(),
                            candidates.len()
                        ),
                    );
                }
                for (index, (case, candidate)) in
                    cases.iter().zip(candidates.iter().copied()).enumerate()
                {
                    if usize::try_from(case.variant).ok() != Some(index) {
                        self.error(
                            ValidationCode::InstructionShape,
                            format!("{path}.case[{index}].variant"),
                            format!(
                                "dynamic switch cases must be ordered 0..n, found variant {}",
                                case.variant
                            ),
                        );
                    }
                    self.validate_forwarded_target_shape(
                        function,
                        case.block,
                        &case.arguments,
                        1,
                        format!("{path}.case[{index}]"),
                    );
                    if let Some(parameter) = function
                        .block(case.block)
                        .and_then(|block| block.params().first())
                    {
                        self.require_value_type(
                            function,
                            *parameter,
                            candidate,
                            ValidationCode::BlockArgument,
                            format!("{path}.case[{index}].payload[0]"),
                        );
                    }
                }
            }
            TerminatorKind::Return(value) => {
                self.require_value_type(
                    function,
                    *value,
                    function.signature.result(),
                    ValidationCode::ReturnType,
                    format!("{path}.value"),
                );
            }
            TerminatorKind::TaskSleep {
                milliseconds,
                normal,
                fault,
            } => {
                self.require_known_value_type(
                    function,
                    *milliseconds,
                    self.scalar_type(&Type::Int),
                    ValidationCode::TypeMismatch,
                    format!("{path}.milliseconds"),
                );
                let task_unit = self
                    .program
                    .representations
                    .type_id(&Type::Task(Box::new(Type::Unit)));
                let canonical_task_unit = task_unit.is_some_and(|ty| {
                    self.program
                        .representations
                        .value_type(ty)
                        .is_some_and(|value_type| {
                            value_type.semantic() == &Type::Task(Box::new(Type::Unit))
                                && self.program.representations.repr(value_type.repr())
                                    == Some(&Repr::TaskHandle)
                        })
                });
                if !canonical_task_unit {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.normal"),
                        "task.sleep result requires the canonical Task[Unit] task-handle representation",
                    );
                }
                self.validate_result_target(
                    function,
                    normal,
                    &[canonical_task_unit.then_some(task_unit).flatten()],
                    &format!("{path}.normal"),
                );
                self.validate_unwind_target(function, fault, &[], &format!("{path}.fault"));
                self.require_may_fault_effect(function, &path, "task.sleep");
                if !function.effects().contains(Effects::NEEDS_EXECUTOR) {
                    self.error(
                        ValidationCode::EffectMismatch,
                        &path,
                        "task.sleep requires the function's NEEDS_EXECUTOR effect",
                    );
                }
            }
            TerminatorKind::AwaitTasks {
                state,
                mode,
                tasks,
                normal,
                fault,
                cancel,
            } => {
                if *state == 0 {
                    self.error(
                        ValidationCode::InvalidCoroutinePlan,
                        format!("{path}.state"),
                        "await_tasks resume state must be nonzero",
                    );
                }
                if tasks.is_empty() {
                    self.error(
                        ValidationCode::InstructionShape,
                        format!("{path}.tasks"),
                        "await_tasks requires at least one child Task",
                    );
                }
                if tasks.iter().copied().collect::<BTreeSet<_>>().len() != tasks.len() {
                    self.error(
                        ValidationCode::InstructionShape,
                        format!("{path}.tasks"),
                        "await_tasks cannot consume the same child Task more than once",
                    );
                }
                let (task_types, outputs): (Vec<_>, Vec<_>) = tasks
                    .iter()
                    .copied()
                    .enumerate()
                    .map(|(index, task)| {
                        let task_type = function.value(task).map(crate::Value::ty);
                        let output = self.task_output_type(function, task);
                        if task_type.is_some() && output.is_none() {
                            self.error(
                                ValidationCode::TypeMismatch,
                                format!("{path}.task[{index}]"),
                                "await_tasks operand must be a canonical concrete Task handle",
                            );
                        }
                        (task_type.filter(|_| output.is_some()), output)
                    })
                    .unzip();
                let result_outputs = match mode {
                    AwaitMode::All => outputs.clone(),
                    AwaitMode::Any => {
                        let first = outputs.first().copied().flatten();
                        if outputs
                            .iter()
                            .copied()
                            .flatten()
                            .any(|output| Some(output) != first)
                        {
                            self.error(
                                ValidationCode::TypeMismatch,
                                format!("{path}.tasks"),
                                "await_tasks any requires every child Task to have the same output type",
                            );
                        }
                        vec![first]
                    }
                    AwaitMode::Settled => task_types,
                    AwaitMode::Race => {
                        let first = outputs.first().copied().flatten();
                        if outputs
                            .iter()
                            .copied()
                            .flatten()
                            .any(|output| Some(output) != first)
                        {
                            self.error(
                                ValidationCode::TypeMismatch,
                                format!("{path}.tasks"),
                                "await_tasks race requires every child Task to have the same output type",
                            );
                        }
                        vec![task_types.first().copied().flatten()]
                    }
                };
                self.validate_result_target(
                    function,
                    normal,
                    &result_outputs,
                    &format!("{path}.normal"),
                );
                self.validate_terminal_task_take_prefix(
                    function,
                    *mode,
                    result_outputs.len(),
                    normal,
                    &path,
                );
                self.validate_unwind_target(function, fault, &[], &format!("{path}.fault"));
                self.validate_target(function, cancel, format!("{path}.cancel"));
                if normal.arguments.as_ref() != fault.arguments.as_ref()
                    || normal.arguments.as_ref() != cancel.arguments.as_ref()
                {
                    self.error(
                        ValidationCode::InvalidCoroutinePlan,
                        &path,
                        "await_tasks normal, fault, and cancel exits must forward the same exact live-value row",
                    );
                }
                if function.coroutine().is_none() {
                    self.error(
                        ValidationCode::InvalidCoroutinePlan,
                        &path,
                        "await_tasks is only valid in a checked coroutine",
                    );
                }
                if !function.effects().contains(Effects::MAY_SUSPEND) {
                    self.error(
                        ValidationCode::EffectMismatch,
                        &path,
                        "await_tasks requires the function's MAY_SUSPEND effect",
                    );
                }
                self.require_may_fault_effect(function, &path, "await_tasks child-fault exit");
            }
            TerminatorKind::CheckedIntNegate {
                value,
                normal,
                fault,
            } => {
                let integer = self.scalar_type(&Type::Int);
                self.require_known_value_type(
                    function,
                    *value,
                    integer,
                    ValidationCode::TypeMismatch,
                    format!("{path}.value"),
                );
                self.validate_result_target(
                    function,
                    normal,
                    &[integer],
                    &format!("{path}.normal"),
                );
                self.validate_unwind_target(function, fault, &[], &format!("{path}.fault"));
                self.require_may_fault_effect(function, &path, "checked integer negate");
            }
            TerminatorKind::CheckedIntBinary {
                left,
                right,
                normal,
                fault,
                ..
            } => {
                let integer = self.scalar_type(&Type::Int);
                self.require_known_value_type(
                    function,
                    *left,
                    integer,
                    ValidationCode::TypeMismatch,
                    format!("{path}.left"),
                );
                self.require_known_value_type(
                    function,
                    *right,
                    integer,
                    ValidationCode::TypeMismatch,
                    format!("{path}.right"),
                );
                self.validate_result_target(
                    function,
                    normal,
                    &[integer],
                    &format!("{path}.normal"),
                );
                self.validate_unwind_target(function, fault, &[], &format!("{path}.fault"));
                self.require_may_fault_effect(function, &path, "checked integer binary operation");
            }
            TerminatorKind::Invoke {
                callee,
                arguments,
                normal,
                unwind,
            } => {
                let callee_id = *callee;
                let callee = self.program.function(callee_id);
                if callee.is_none() {
                    self.error(
                        ValidationCode::InvalidFunctionReference,
                        format!("{path}.callee"),
                        format!("callee {callee_id} does not exist"),
                    );
                }
                if let Some(effects) = self.exact_effect(callee_id) {
                    if !effects.contains(Effects::MAY_FAULT) {
                        self.error(
                            ValidationCode::CallShape,
                            format!("{path}.callee"),
                            "invoke requires a may-fault callee; use direct call for an infallible callee",
                        );
                    }
                    if effects.contains(Effects::MAY_SUSPEND) {
                        self.error(
                            ValidationCode::CallShape,
                            format!("{path}.callee"),
                            "invoke cannot target a suspending callee",
                        );
                    }
                }
                if let Some(callee) = callee {
                    if callee.coroutine().is_some() {
                        self.error(
                            ValidationCode::CallShape,
                            format!("{path}.callee"),
                            "invoke cannot target a coroutine constructor; use task.create",
                        );
                    }
                    self.validate_call_arguments(
                        function,
                        arguments,
                        callee,
                        &format!("{path}.argument"),
                    );
                    let mut normal_types =
                        Vec::with_capacity(1 + callee.signature.inout_params().len());
                    normal_types.push(Some(callee.signature.result()));
                    normal_types.extend(
                        Self::signature_writeback_types(callee)
                            .into_iter()
                            .map(Some),
                    );
                    self.validate_result_target(
                        function,
                        normal,
                        &normal_types,
                        &format!("{path}.normal"),
                    );
                    let unwind_types = Self::signature_writeback_types(callee)
                        .into_iter()
                        .map(Some)
                        .collect::<Vec<_>>();
                    self.validate_unwind_target(
                        function,
                        unwind,
                        &unwind_types,
                        &format!("{path}.unwind"),
                    );
                } else {
                    self.validate_result_target(
                        function,
                        normal,
                        &[None],
                        &format!("{path}.normal"),
                    );
                    self.validate_unwind_target(function, unwind, &[], &format!("{path}.unwind"));
                }
                self.require_may_fault_effect(function, &path, "invoke");
            }
            TerminatorKind::LogWrite {
                level,
                message,
                fields,
                normal,
                fault,
            } => {
                let level_semantic = self
                    .program
                    .canonical_types
                    .log_level
                    .map(|level| Type::Nominal(level, Vec::new()));
                let level_type = level_semantic
                    .as_ref()
                    .and_then(|semantic| self.program.representations.type_id(semantic));
                let canonical_level = level_type.is_some_and(|ty| {
                    let Some(value_type) = self.program.representations.value_type(ty) else {
                        return false;
                    };
                    if value_type.kind() != ValueTypeKind::Direct
                        || Some(value_type.semantic()) != level_semantic.as_ref()
                        || self.program.representations.type_id(value_type.semantic()) != Some(ty)
                    {
                        return false;
                    }
                    let Repr::Sum(sum) = self
                        .program
                        .representations
                        .repr(value_type.repr())
                        .copied()
                        .unwrap_or(Repr::Uninhabited)
                    else {
                        return false;
                    };
                    self.program.representations.sum(sum).is_some_and(|sum| {
                        sum.tag() == SumTagRepr::I8
                            && sum.variants().len() == 4
                            && sum
                                .variants()
                                .iter()
                                .all(|variant| variant.fields().is_empty())
                    })
                });
                if !canonical_level {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.level"),
                        "typed logging requires the cataloged canonical LogLevel with four empty ordered variants",
                    );
                }
                self.require_known_value_type(
                    function,
                    *level,
                    level_type,
                    ValidationCode::TypeMismatch,
                    format!("{path}.level"),
                );

                let text = self.scalar_type(&Type::Text);
                self.require_known_value_type(
                    function,
                    *message,
                    text,
                    ValidationCode::TypeMismatch,
                    format!("{path}.message"),
                );
                let fields_semantic = self
                    .program
                    .canonical_types
                    .text_map
                    .map(|text_map| Type::Nominal(text_map, vec![Type::Text]));
                let fields_type = fields_semantic
                    .as_ref()
                    .and_then(|semantic| self.program.representations.type_id(semantic));
                let canonical_fields = fields_type.is_some_and(|ty| {
                    self.program
                        .representations
                        .value_type(ty)
                        .is_some_and(|value_type| {
                            value_type.kind() == ValueTypeKind::ManagedTextMap
                                && Some(value_type.semantic()) == fields_semantic.as_ref()
                                && self.program.representations.type_id(value_type.semantic())
                                    == Some(ty)
                                && self.program.representations.repr(value_type.repr())
                                    == Some(&Repr::ManagedPointer)
                        })
                        && self.text_map_value(ty) == text
                });
                if !canonical_fields {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.fields"),
                        "typed structured logging requires cataloged canonical TextMap[Text] fields",
                    );
                }
                self.require_known_value_type(
                    function,
                    *fields,
                    fields_type,
                    ValidationCode::TypeMismatch,
                    format!("{path}.fields"),
                );
                self.validate_result_target(
                    function,
                    normal,
                    &[self.scalar_type(&Type::Unit)],
                    &format!("{path}.normal"),
                );
                self.validate_unwind_target(function, fault, &[], &format!("{path}.fault"));
                self.require_may_fault_effect(function, &path, "typed log write");
            }
            TerminatorKind::StdoutWrite {
                text,
                normal,
                fault,
            } => {
                self.require_known_value_type(
                    function,
                    *text,
                    self.scalar_type(&Type::Text),
                    ValidationCode::TypeMismatch,
                    format!("{path}.text"),
                );
                self.validate_result_target(
                    function,
                    normal,
                    &[self.scalar_type(&Type::Unit)],
                    &format!("{path}.normal"),
                );
                self.validate_unwind_target(function, fault, &[], &format!("{path}.fault"));
                self.require_may_fault_effect(function, &path, "standard-output write");
            }
            TerminatorKind::Assert {
                condition,
                metadata,
                success,
                fault,
            } => {
                self.require_known_value_type(
                    function,
                    *condition,
                    self.scalar_type(&Type::Bool),
                    ValidationCode::TypeMismatch,
                    format!("{path}.condition"),
                );
                self.validate_target(function, success, format!("{path}.success"));
                self.validate_unwind_target(function, fault, &[], &format!("{path}.fault"));
                self.validate_fault_metadata(function, metadata, &format!("{path}.metadata"));
                self.require_may_fault_effect(function, &path, "assert");
            }
            TerminatorKind::Fault { metadata } => {
                self.validate_fault_metadata(function, metadata, &format!("{path}.metadata"));
                self.require_may_fault_effect(function, &path, "fault");
            }
            TerminatorKind::ResumeFault => {
                self.require_may_fault_effect(function, &path, "resume_fault");
            }
            TerminatorKind::TaskCancelled => {
                if function.coroutine().is_none() {
                    self.error(
                        ValidationCode::InvalidCoroutinePlan,
                        &path,
                        "task.cancelled is only valid in a checked coroutine",
                    );
                }
            }
        }
    }

    fn validate_target(&mut self, function: &Function, target: &crate::BlockTarget, path: String) {
        self.validate_forwarded_target(function, target.block, &target.arguments, &[], path);
    }

    fn validate_result_target(
        &mut self,
        function: &Function,
        target: &ResultTarget,
        implicit_types: &[Option<ValueTypeId>],
        path: &str,
    ) {
        self.validate_forwarded_target(
            function,
            target.block,
            &target.arguments,
            implicit_types,
            path.to_owned(),
        );
        let Some(block) = function.block(target.block) else {
            return;
        };
        for (index, (parameter, expected)) in block
            .params
            .iter()
            .copied()
            .zip(implicit_types.iter().copied())
            .enumerate()
        {
            if let Some(expected) = expected {
                self.require_value_type(
                    function,
                    parameter,
                    expected,
                    ValidationCode::BlockArgument,
                    format!("{path}.implicit[{index}]"),
                );
            }
        }
    }

    fn validate_sum_case(
        &mut self,
        function: &Function,
        case: &crate::SumCase,
        sum: SumReprId,
        variant: usize,
        path: &str,
    ) {
        let implicit_count = self
            .sum_variant_field_count(sum, variant)
            .unwrap_or_default();
        self.validate_forwarded_target_shape(
            function,
            case.block,
            &case.arguments,
            implicit_count,
            path.to_owned(),
        );
        let Some(block) = function.block(case.block) else {
            return;
        };
        for (index, parameter) in block
            .params
            .iter()
            .copied()
            .take(implicit_count)
            .enumerate()
        {
            let Some(expected) = self.sum_variant_field(sum, variant, index) else {
                continue;
            };
            self.require_value_type(
                function,
                parameter,
                expected,
                ValidationCode::BlockArgument,
                format!("{path}.payload[{index}]"),
            );
        }
    }

    fn validate_unwind_target(
        &mut self,
        function: &Function,
        target: &UnwindTarget,
        implicit_types: &[Option<ValueTypeId>],
        path: &str,
    ) {
        self.validate_forwarded_target(
            function,
            target.block,
            &target.arguments,
            implicit_types,
            path.to_owned(),
        );
        let Some(block) = function.block(target.block) else {
            return;
        };
        for (index, (parameter, expected)) in block
            .params
            .iter()
            .copied()
            .zip(implicit_types.iter().copied())
            .enumerate()
        {
            if let Some(expected) = expected {
                self.require_value_type(
                    function,
                    parameter,
                    expected,
                    ValidationCode::BlockArgument,
                    format!("{path}.implicit[{index}]"),
                );
            }
        }
    }

    fn validate_forwarded_target(
        &mut self,
        function: &Function,
        target_block: BlockId,
        arguments: &[ValueId],
        implicit_types: &[Option<ValueTypeId>],
        path: String,
    ) {
        self.validate_forwarded_target_shape(
            function,
            target_block,
            arguments,
            implicit_types.len(),
            path,
        );
    }

    fn validate_forwarded_target_shape(
        &mut self,
        function: &Function,
        target_block: BlockId,
        arguments: &[ValueId],
        implicit_count: usize,
        path: String,
    ) {
        let Some(block) = function.block(target_block) else {
            self.error(
                ValidationCode::InvalidBlockReference,
                path,
                format!("target block {target_block} does not exist"),
            );
            return;
        };
        let parameter_offset = implicit_count;
        let supplied = arguments.len().saturating_add(parameter_offset);
        if supplied != block.params.len() {
            self.error(
                ValidationCode::BlockArgument,
                path.clone(),
                format!(
                    "edge supplies {supplied} parameter value(s), including {parameter_offset} implicit result(s); target {} requires {}",
                    target_block,
                    block.params.len()
                ),
            );
        }
        for (index, (argument, parameter)) in arguments
            .iter()
            .copied()
            .zip(block.params.iter().copied().skip(parameter_offset))
            .enumerate()
        {
            let Some(expected) = function.value(parameter).map(|value| value.ty) else {
                continue;
            };
            self.require_value_type(
                function,
                argument,
                expected,
                ValidationCode::BlockArgument,
                format!("{path}.argument[{index}]"),
            );
        }
    }

    fn validate_call_arguments(
        &mut self,
        function: &Function,
        arguments: &[ValueId],
        callee: &Function,
        path: &str,
    ) {
        if arguments.len() != callee.signature.params().len() {
            self.error(
                ValidationCode::CallShape,
                format!("{path}s"),
                format!(
                    "call has {} arguments, callee requires {}",
                    arguments.len(),
                    callee.signature.params().len()
                ),
            );
        }
        for (index, (argument, expected)) in arguments
            .iter()
            .copied()
            .zip(callee.signature.params().iter().copied())
            .enumerate()
        {
            self.require_value_type(
                function,
                argument,
                expected,
                ValidationCode::CallShape,
                format!("{path}[{index}]"),
            );
        }
    }

    fn require_may_fault_effect(&mut self, function: &Function, path: &str, operation: &str) {
        if !function.effects.contains(Effects::MAY_FAULT) {
            self.error(
                ValidationCode::EffectMismatch,
                path,
                format!("{operation} requires the function's MAY_FAULT effect"),
            );
        }
    }

    fn validate_fault_metadata(
        &mut self,
        function: &Function,
        metadata: &crate::FaultMetadata,
        path: &str,
    ) {
        if let crate::FaultMetadata::Contract(metadata) = metadata {
            self.validate_contract_fault_metadata(function, metadata, path);
        }
    }

    fn validate_contract_fault_metadata(
        &mut self,
        function: &Function,
        metadata: &crate::ContractFaultMetadata,
        path: &str,
    ) {
        let user_code_in_budget = self.validate_contract_fault_text(metadata, path);
        self.validate_contract_fault_span(
            metadata.contract_span(),
            &format!("{path}.contract_span"),
        );
        match metadata.blame() {
            crate::ContractFaultBlame::Static(span) => {
                self.validate_contract_fault_span(span, &format!("{path}.blame_span"));
            }
            crate::ContractFaultBlame::CoroutineCallSite => {
                if metadata.kind() != crate::ContractFaultKind::Precondition {
                    self.error(
                        ValidationCode::FaultMetadata,
                        format!("{path}.blame_span"),
                        "only PreconditionFault may blame the coroutine call site",
                    );
                }
                if !function
                    .coroutine()
                    .is_some_and(crate::CoroutinePlan::carries_caller_span)
                {
                    self.error(
                        ValidationCode::FaultMetadata,
                        format!("{path}.blame_span"),
                        "coroutine-call-site blame requires a coroutine plan carrying the caller span",
                    );
                }
            }
        }

        match metadata.kind() {
            crate::ContractFaultKind::Assertion => {
                self.validate_assertion_fault_metadata(metadata, path);
            }
            crate::ContractFaultKind::Precondition
            | crate::ContractFaultKind::Postcondition
            | crate::ContractFaultKind::Invariant => {
                self.validate_named_contract_fault_metadata(metadata, path, user_code_in_budget);
            }
        }
    }

    fn validate_contract_fault_text(
        &mut self,
        metadata: &crate::ContractFaultMetadata,
        path: &str,
    ) -> bool {
        let user_code_in_budget = metadata.user_code().is_none_or(|user_code| {
            if user_code.len() <= crate::CONTRACT_FAULT_TEXT_MAX_BYTES {
                true
            } else {
                self.error(
                    ValidationCode::FaultMetadata,
                    format!("{path}.user_code"),
                    format!(
                        "contract user code is {} UTF-8 bytes, exceeding the {}-byte limit",
                        user_code.len(),
                        crate::CONTRACT_FAULT_TEXT_MAX_BYTES
                    ),
                );
                false
            }
        });
        if metadata.message().len() > crate::CONTRACT_FAULT_TEXT_MAX_BYTES {
            self.error(
                ValidationCode::FaultMetadata,
                format!("{path}.message"),
                format!(
                    "contract fault message is {} UTF-8 bytes, exceeding the {}-byte limit",
                    metadata.message().len(),
                    crate::CONTRACT_FAULT_TEXT_MAX_BYTES
                ),
            );
        }
        user_code_in_budget
    }

    fn validate_contract_fault_span(&mut self, span: loom_core::Span, path: &str) {
        if span.range.start > span.range.end {
            self.error(
                ValidationCode::FaultMetadata,
                path,
                format!(
                    "fault span starts at {}, after its end {}",
                    span.range.start, span.range.end
                ),
            );
        }
    }

    fn validate_assertion_fault_metadata(
        &mut self,
        metadata: &crate::ContractFaultMetadata,
        path: &str,
    ) {
        if metadata.user_code().is_some() {
            self.error(
                ValidationCode::FaultMetadata,
                format!("{path}.user_code"),
                "AssertionFault must not carry a named contract code",
            );
        }
        if metadata.message() != "assertion was not satisfied" {
            self.error(
                ValidationCode::FaultMetadata,
                format!("{path}.message"),
                "AssertionFault message must be `assertion was not satisfied`",
            );
        }
        if metadata.blame() != crate::ContractFaultBlame::Static(metadata.contract_span()) {
            self.error(
                ValidationCode::FaultMetadata,
                format!("{path}.blame_span"),
                "AssertionFault must blame its assertion span",
            );
        }
    }

    fn validate_named_contract_fault_metadata(
        &mut self,
        metadata: &crate::ContractFaultMetadata,
        path: &str,
        user_code_in_budget: bool,
    ) {
        if let Some(user_code) = metadata.user_code() {
            if user_code.is_empty() {
                self.error(
                    ValidationCode::FaultMetadata,
                    format!("{path}.user_code"),
                    format!(
                        "{} requires a non-empty user contract code",
                        metadata.kind().fault_code()
                    ),
                );
            }
            if user_code_in_budget {
                let expected = format!("contract `{user_code}` was not satisfied");
                if metadata.message() != expected {
                    self.error(
                        ValidationCode::FaultMetadata,
                        format!("{path}.message"),
                        format!(
                            "{} message must be derived from its user contract code",
                            metadata.kind().fault_code()
                        ),
                    );
                }
            }
        } else {
            self.error(
                ValidationCode::FaultMetadata,
                format!("{path}.user_code"),
                format!(
                    "{} requires a non-empty user contract code",
                    metadata.kind().fault_code()
                ),
            );
        }
        if metadata.kind() != crate::ContractFaultKind::Precondition
            && metadata.blame() != crate::ContractFaultBlame::Static(metadata.contract_span())
        {
            self.error(
                ValidationCode::FaultMetadata,
                format!("{path}.blame_span"),
                format!(
                    "{} must blame its implementation contract span",
                    metadata.kind().fault_code()
                ),
            );
        }
    }

    fn validate_terminator_fault_state(
        &mut self,
        terminator: &Terminator,
        state: FaultStateSet,
        block_index: usize,
        base: &str,
    ) {
        if state == FaultStateSet::BOTH || state == FaultStateSet::NONE {
            return;
        }
        let path = format!("{base}.block[{block_index}].terminator");
        match terminator.kind() {
            TerminatorKind::Return(_)
            | TerminatorKind::Fault { .. }
            | TerminatorKind::TaskCancelled
                if state == FaultStateSet::ACTIVE =>
            {
                self.error(
                    ValidationCode::FaultState,
                    path,
                    "an active source fault cannot return normally, cancel, or originate a second terminal fault; propagate it with resume_fault",
                );
            }
            TerminatorKind::ResumeFault if state == FaultStateSet::INACTIVE => {
                self.error(
                    ValidationCode::FaultState,
                    path,
                    "resume_fault requires an active source fault from an unwind edge",
                );
            }
            TerminatorKind::AwaitTasks { .. } if state == FaultStateSet::ACTIVE => {
                self.error(
                    ValidationCode::FaultState,
                    path,
                    "source-fault cleanup cannot suspend again; finish with resume_fault",
                );
            }
            _ => {}
        }
    }

    /// Keeps cancellation as an explicit coroutine-CFG obligation. A backend
    /// may dispatch a requested cancellation directly to an `await_tasks`
    /// cancel target, so that path cannot merge back into ordinary execution,
    /// suspend again, or manufacture `task.cancelled` from a normal path.
    fn validate_block_cancellation_state(
        &mut self,
        function: &Function,
        block: &crate::Block,
        terminator: &Terminator,
        state: CancellationStateSet,
        block_index: usize,
        base: &str,
    ) {
        if state == CancellationStateSet::NONE {
            return;
        }
        if state.contains(CancellationStateSet::ACTIVE) {
            for instruction_id in block.instructions() {
                let Some(instruction) = function.instruction(*instruction_id) else {
                    continue;
                };
                let operation = match instruction.kind() {
                    InstructionKind::TaskCreate { .. } => Some("create a Task"),
                    InstructionKind::IoTaskCreate { .. } => Some("create a typed I/O Task"),
                    InstructionKind::TaskJoin { .. } => Some("construct a Task join"),
                    InstructionKind::TaskJoinList { .. } => {
                        Some("construct a runtime-width Task join")
                    }
                    InstructionKind::TaskOutcomeTake { .. } => {
                        Some("consume a terminal Task outcome")
                    }
                    InstructionKind::DirectCall { callee, .. }
                        if self.cancellation_call_changes_scheduler_topology(*callee) =>
                    {
                        Some("call an executor-dependent function")
                    }
                    _ => None,
                };
                if let Some(operation) = operation {
                    self.error(
                        ValidationCode::InvalidCoroutinePlan,
                        format!(
                            "{base}.block[{block_index}].instruction[{}]",
                            instruction_id.raw()
                        ),
                        format!(
                            "cancellation cleanup cannot {operation}; it must remain scheduler-topology neutral"
                        ),
                    );
                }
            }
        }
        let path = format!("{base}.block[{block_index}].terminator");
        match terminator.kind() {
            TerminatorKind::TaskCancelled if state.contains(CancellationStateSet::INACTIVE) => {
                self.error(
                    ValidationCode::InvalidCoroutinePlan,
                    path,
                    "task.cancelled requires an active cancellation from an await_tasks cancel edge",
                );
            }
            TerminatorKind::Return(_) if state.contains(CancellationStateSet::ACTIVE) => {
                self.error(
                    ValidationCode::InvalidCoroutinePlan,
                    path,
                    "an active coroutine cancellation cannot return normally; finish cleanup with task.cancelled",
                );
            }
            TerminatorKind::TaskSleep { .. } | TerminatorKind::AwaitTasks { .. }
                if state.contains(CancellationStateSet::ACTIVE) =>
            {
                self.error(
                    ValidationCode::InvalidCoroutinePlan,
                    path,
                    "cancellation cleanup cannot create or await a Task; it must remain scheduler-topology neutral",
                );
            }
            TerminatorKind::Invoke { callee, .. }
                if state.contains(CancellationStateSet::ACTIVE)
                    && self.cancellation_call_changes_scheduler_topology(*callee) =>
            {
                self.error(
                    ValidationCode::InvalidCoroutinePlan,
                    path,
                    "cancellation cleanup cannot invoke an executor-dependent function; it must remain scheduler-topology neutral",
                );
            }
            _ => {}
        }
    }

    fn cancellation_call_changes_scheduler_topology(&self, function: InstanceId) -> bool {
        match self.exact_effect(function) {
            None => true,
            Some(effects) => {
                effects.contains(Effects::NEEDS_EXECUTOR)
                    && !self.executor_dependencies_are_resource_close_only(function)
            }
        }
    }

    /// Resource release needs executor access without changing its Task graph.
    /// Walk the complete synchronous call closure so a wrapper cannot hide a
    /// Task operation. Visiting each instance once handles recursive SCCs while
    /// still inspecting every member; unresolved callees remain conservatively
    /// unsafe.
    fn executor_dependencies_are_resource_close_only(&self, root: InstanceId) -> bool {
        let Some(root) = canonical_function_index(self.program, root) else {
            return false;
        };
        let mut pending = vec![root];
        let mut visited = vec![false; self.program.functions.len()];
        while let Some(index) = pending.pop() {
            let Some(was_visited) = visited.get_mut(index) else {
                return false;
            };
            if *was_visited {
                continue;
            }
            *was_visited = true;

            let Some(function) = self.program.functions.get(index) else {
                return false;
            };
            if function.entry.is_none() || function.coroutine().is_some() {
                return false;
            }
            for block in &function.blocks {
                for instruction_id in block.instructions() {
                    let Some(instruction) = function.instruction(*instruction_id) else {
                        return false;
                    };
                    if instruction_direct_effects(instruction.kind())
                        .contains(Effects::NEEDS_EXECUTOR)
                        && !matches!(instruction.kind(), InstructionKind::ResourceClose { .. })
                    {
                        return false;
                    }
                    if let InstructionKind::DirectCall { callee, .. } = instruction.kind() {
                        let Some(callee) = canonical_function_index(self.program, *callee) else {
                            return false;
                        };
                        pending.push(callee);
                    }
                }

                let Some(terminator) = block.terminator() else {
                    return false;
                };
                match terminator.kind() {
                    TerminatorKind::TaskSleep { .. } | TerminatorKind::AwaitTasks { .. } => {
                        return false;
                    }
                    TerminatorKind::Invoke { callee, .. } => {
                        let Some(callee) = canonical_function_index(self.program, *callee) else {
                            return false;
                        };
                        pending.push(callee);
                    }
                    _ => {}
                }
            }
        }
        true
    }

    fn exact_effect(&self, function: InstanceId) -> Option<Effects> {
        canonical_function_index(self.program, function)
            .and_then(|index| self.exact_effects.get(index).copied())
    }

    /// Validates the edge-sensitive fact consumed by `int.successor_below`.
    ///
    /// A proof value must drive exactly one reachable branch. Its true target
    /// must have that branch as its only reachable predecessor and dominate
    /// every proof use. The unique-entry rule turns ordinary block dominance
    /// into true-edge dominance without materializing path facts per block.
    fn validate_integer_proofs(
        &mut self,
        function: &Function,
        base: &str,
        reachable: &[bool],
        predecessors: &[Vec<usize>],
        dominators: &DominatorTree,
    ) {
        let facts = IntegerProofFacts::collect(function, reachable, predecessors);
        for (block_index, block) in function.blocks.iter().enumerate() {
            if !reachable.get(block_index).copied().unwrap_or(false) {
                continue;
            }
            for instruction_id in &block.instructions {
                let Some(instruction) = function.instruction(*instruction_id) else {
                    continue;
                };
                let InstructionKind::IntSuccessorBelow {
                    value,
                    upper_bound,
                    proof,
                } = &instruction.kind
                else {
                    continue;
                };
                let (value, upper_bound, proof) = (*value, *upper_bound, *proof);
                self.validate_integer_successor(
                    function,
                    instruction,
                    block_index,
                    value,
                    upper_bound,
                    proof,
                    &facts,
                    dominators,
                    base,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn validate_integer_successor(
        &mut self,
        function: &Function,
        instruction: &Instruction,
        block_index: usize,
        value: ValueId,
        upper_bound: ValueId,
        proof: ValueId,
        facts: &IntegerProofFacts,
        dominators: &DominatorTree,
        base: &str,
    ) {
        let path = format!("{base}.instruction[{}].proof", instruction.id.index());
        if !is_exact_successor_proof(function, value, upper_bound, proof) {
            self.error(
                ValidationCode::InvalidIntegerProof,
                path,
                "integer successor proof must be the exact result of value < upper_bound",
            );
            return;
        }
        let branch = (proof.owner() == function.id)
            .then(|| facts.branches.get(proof.index()).copied())
            .flatten()
            .unwrap_or(ProofBranch::None);
        let (source, true_target) = match branch {
            ProofBranch::Unique {
                source,
                true_target,
            } => (source, true_target),
            ProofBranch::None => {
                self.error(
                    ValidationCode::InvalidIntegerProof,
                    path,
                    "integer successor proof must condition a reachable branch",
                );
                return;
            }
            ProofBranch::Ambiguous => {
                self.error(
                    ValidationCode::InvalidIntegerProof,
                    path,
                    "integer successor proof cannot condition multiple reachable branches",
                );
                return;
            }
        };
        if facts.predecessors.get(true_target) != Some(&UniquePredecessor::One(source)) {
            self.error(
                ValidationCode::InvalidIntegerProof,
                path,
                "comparison true target must have only the proving branch as a reachable predecessor",
            );
            return;
        }
        if !dominators.dominates(true_target, block_index) {
            self.error(
                ValidationCode::InvalidIntegerProof,
                path,
                "comparison true edge does not dominate the integer successor",
            );
        }
    }

    fn validate_dominance(
        &mut self,
        function: &Function,
        base: &str,
        schedule: &[Option<(BlockId, usize)>],
        reachable: &[bool],
        dominators: &DominatorTree,
    ) {
        for (block_index, block) in function.blocks.iter().enumerate() {
            if !reachable.get(block_index).copied().unwrap_or(false) {
                continue;
            }
            let Some(use_block) = BlockId::from_index(function.id, block_index) else {
                continue;
            };
            for (position, instruction_id) in block.instructions.iter().copied().enumerate() {
                let Some(instruction) = function.instruction(instruction_id) else {
                    continue;
                };
                for (operand_index, operand) in instruction.kind.operands().into_iter().enumerate()
                {
                    self.require_dominance(
                        function,
                        operand,
                        use_block,
                        position,
                        schedule,
                        dominators,
                        format!(
                            "{base}.block[{block_index}].instruction[{position}].operand[{operand_index}]"
                        ),
                    );
                }
            }
            if let Some(terminator) = &block.terminator {
                for (operand_index, operand) in terminator.operands().into_iter().enumerate() {
                    self.require_dominance(
                        function,
                        operand,
                        use_block,
                        block.instructions.len(),
                        schedule,
                        dominators,
                        format!("{base}.block[{block_index}].terminator.operand[{operand_index}]"),
                    );
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn require_dominance(
        &mut self,
        function: &Function,
        value: ValueId,
        use_block: BlockId,
        use_position: usize,
        schedule: &[Option<(BlockId, usize)>],
        dominators: &DominatorTree,
        path: String,
    ) {
        let Some(value) = function.value(value) else {
            self.error(
                ValidationCode::InvalidValueReference,
                path,
                format!("operand {value} does not exist"),
            );
            return;
        };
        let (definition_block, definition_position) = match value.definition {
            ValueDefinition::BlockParameter { block, .. } => (block, None),
            ValueDefinition::InstructionResult { instruction, .. } => {
                if instruction.owner() != function.id {
                    return;
                }
                let Some(Some((block, position))) = schedule.get(instruction.index()) else {
                    return;
                };
                (*block, Some(*position))
            }
        };
        let dominates = if definition_block == use_block {
            definition_position.is_none_or(|position| position < use_position)
        } else {
            dominators.dominates(definition_block.index(), use_block.index())
        };
        if !dominates {
            self.error(
                ValidationCode::Dominance,
                path,
                format!("{} does not dominate its use in {}", value.id, use_block),
            );
        }
    }

    fn require_results(
        &mut self,
        function: &Function,
        instruction: &Instruction,
        expected: &[Option<ValueTypeId>],
        path: &str,
    ) {
        if instruction.results.len() != expected.len() {
            self.error(
                ValidationCode::InstructionShape,
                format!("{path}.results"),
                format!(
                    "instruction has {} results, operation requires {}",
                    instruction.results.len(),
                    expected.len()
                ),
            );
        }
        for (index, (value, expected)) in instruction
            .results
            .iter()
            .copied()
            .zip(expected.iter().copied())
            .enumerate()
        {
            if let Some(expected) = expected {
                self.require_value_type(
                    function,
                    value,
                    expected,
                    ValidationCode::TypeMismatch,
                    format!("{path}.result[{index}]"),
                );
            }
        }
    }

    fn require_known_value_type(
        &mut self,
        function: &Function,
        value: ValueId,
        expected: Option<ValueTypeId>,
        code: ValidationCode,
        path: String,
    ) {
        if let Some(expected) = expected {
            self.require_value_type(function, value, expected, code, path);
        }
    }

    fn require_value_type(
        &mut self,
        function: &Function,
        value: ValueId,
        expected: ValueTypeId,
        code: ValidationCode,
        path: String,
    ) {
        let Some(value) = function.value(value) else {
            self.error(
                ValidationCode::InvalidValueReference,
                path,
                format!("value {value} does not exist"),
            );
            return;
        };
        if value.ty != expected {
            self.error(
                code,
                path,
                format!("{} has type {}, expected {expected}", value.id, value.ty),
            );
        }
    }

    fn scalar_type(&self, semantic: &Type) -> Option<ValueTypeId> {
        self.program.representations.type_id(semantic)
    }

    fn managed_bytes_type(&self) -> Option<ValueTypeId> {
        let canonical = self.program.canonical_types.bytes?;
        let bytes = self
            .program
            .representations
            .type_id(&Type::Nominal(canonical, Vec::new()))?;
        self.program
            .representations
            .is_managed_bytes_type(Some(canonical), bytes)
            .then_some(bytes)
    }

    fn canonical_path_type(&self) -> Option<(ValueTypeId, ValueTypeId)> {
        let text = self.scalar_type(&Type::Text)?;
        let text_type = self.program.representations.value_type(text)?;
        if text_type.kind() != ValueTypeKind::Direct
            || self.program.representations.repr(text_type.repr()) != Some(&Repr::ManagedPointer)
        {
            return None;
        }
        let semantic = Type::Nominal(self.program.canonical_types.path?, Vec::new());
        let path = self.scalar_type(&semantic)?;
        let path_type = self.program.representations.value_type(path)?;
        (path_type.kind() == ValueTypeKind::InvariantProduct
            && self.product_fields(path) == Some(&[text]))
        .then_some((path, text))
    }

    fn canonical_resource_type(&self, kind: crate::ResourceKind) -> Option<ValueTypeId> {
        let identity = match kind {
            crate::ResourceKind::File => self.program.canonical_types.file?,
            crate::ResourceKind::Socket => self.program.canonical_types.socket?,
        };
        let semantic = Type::Nominal(identity, Vec::new());
        let resource = self.scalar_type(&semantic)?;
        let mut registrations = self
            .program
            .representations
            .registrations()
            .iter()
            .filter(|registration| registration.semantic() == &semantic);
        let registration = registrations.next()?;
        if registration.value_type() != resource || registrations.next().is_some() {
            return None;
        }
        let value_type = self.program.representations.value_type(resource)?;
        let integer = self.scalar_type(&Type::Int)?;
        (value_type.semantic() == &semantic
            && value_type.kind() == ValueTypeKind::Direct
            && self.product_fields(resource) == Some(&[integer]))
        .then_some(resource)
    }

    fn canonical_io_error_type(&self) -> Option<ValueTypeId> {
        let semantic = Type::Nominal(self.program.canonical_types.io_error?, Vec::new());
        let error = self.program.representations.type_id(&semantic)?;
        let error_value = self.program.representations.value_type(error)?;
        let kind_semantic = Type::Nominal(self.program.canonical_types.io_error_kind?, Vec::new());
        let kind = self.program.representations.type_id(&kind_semantic)?;
        let text = self.scalar_type(&Type::Text)?;
        let text_value = self.program.representations.value_type(text)?;
        if error_value.semantic() != &semantic
            || error_value.kind() != ValueTypeKind::Direct
            || self.program.representations.repr(text_value.repr()) != Some(&Repr::ManagedPointer)
            || self.product_fields(error) != Some(&[kind, text])
        {
            return None;
        }
        let sum = self.sum_repr(kind)?;
        let sum = self.program.representations.sum(sum)?;
        (sum.tag() == SumTagRepr::I8
            && sum.variants().len() == 10
            && sum
                .variants()
                .iter()
                .all(|variant| variant.fields().is_empty()))
        .then_some(error)
    }

    fn canonical_task_handle(&self, task: ValueTypeId, output: ValueTypeId) -> bool {
        let Some(task_type) = self.program.representations.value_type(task) else {
            return false;
        };
        let Some(output_type) = self.program.representations.value_type(output) else {
            return false;
        };
        task_type.kind() == ValueTypeKind::Direct
            && self.program.representations.repr(task_type.repr()) == Some(&Repr::TaskHandle)
            && task_type.semantic() == &Type::Task(Box::new(output_type.semantic().clone()))
            && self.program.representations.type_id(task_type.semantic()) == Some(task)
    }

    /// Resolves one cataloged `TaskOutcome[T]` only after rechecking the exact
    /// runtime-visible sum and fault payload contract. First-class `settled`
    /// and `race` joins publish this value from scheduler callbacks, so their
    /// result type needs the same independent proof as `task.outcome_take`.
    fn canonical_task_outcome_type(&self, output: ValueTypeId) -> Option<ValueTypeId> {
        let output_semantic = self
            .program
            .representations
            .value_type(output)?
            .semantic()
            .clone();
        let outcome_semantic = Type::Nominal(
            self.program.canonical_types.task_outcome?,
            vec![output_semantic],
        );
        let outcome = self.program.representations.type_id(&outcome_semantic)?;
        let outcome_value = self.program.representations.value_type(outcome)?;
        if outcome_value.kind() != ValueTypeKind::Direct
            || outcome_value.semantic() != &outcome_semantic
            || self.program.representations.type_id(&outcome_semantic) != Some(outcome)
        {
            return None;
        }

        let text = self.scalar_type(&Type::Text)?;
        let text_value = self.program.representations.value_type(text)?;
        if text_value.kind() != ValueTypeKind::Direct
            || text_value.semantic() != &Type::Text
            || self.program.representations.repr(text_value.repr()) != Some(&Repr::ManagedPointer)
        {
            return None;
        }
        let fault_semantic = Type::Nominal(self.program.canonical_types.task_fault?, Vec::new());
        let fault = self.program.representations.type_id(&fault_semantic)?;
        let fault_value = self.program.representations.value_type(fault)?;
        if fault_value.kind() != ValueTypeKind::Direct
            || fault_value.semantic() != &fault_semantic
            || self.program.representations.type_id(&fault_semantic) != Some(fault)
            || self.product_fields(fault) != Some(&[text, text])
        {
            return None;
        }

        let variants = self
            .sum_repr(outcome)
            .and_then(|sum| self.program.representations.sum(sum))?
            .variants();
        (variants.len() == 3
            && variants[TASK_OUTCOME_COMPLETED_VARIANT as usize].fields() == [output]
            && variants[TASK_OUTCOME_FAULTED_VARIANT as usize].fields() == [fault]
            && variants[TASK_OUTCOME_CANCELLED_VARIANT as usize]
                .fields()
                .is_empty())
        .then_some(outcome)
    }

    fn is_resource_capability_type(&self, ty: ValueTypeId) -> bool {
        self.program
            .representations
            .value_type(ty)
            .is_some_and(|value_type| {
                matches!(value_type.semantic(), Type::Nominal(id, arguments)
                    if arguments.is_empty()
                        && (Some(*id) == self.program.canonical_types.file
                            || Some(*id) == self.program.canonical_types.socket))
            })
    }

    fn canonical_io_result_shape(
        &self,
        result: ValueTypeId,
        success: Option<ValueTypeId>,
        io_error: Option<ValueTypeId>,
    ) -> bool {
        let (Some(success), Some(io_error), Some(sum)) = (success, io_error, self.sum_repr(result))
        else {
            return false;
        };
        let Some(sum) = self.program.representations.sum(sum) else {
            return false;
        };
        sum.variants().len() == 2
            && sum.variants()[0].fields() == [success]
            && sum.variants()[1].fields() == [io_error]
    }

    fn product_fields(&self, ty: ValueTypeId) -> Option<&[ValueTypeId]> {
        let value_type = self.program.representations.value_type(ty)?;
        let Repr::Product(product) = self
            .program
            .representations
            .repr(value_type.repr())
            .copied()?
        else {
            return None;
        };
        self.program
            .representations
            .product(product)
            .map(crate::ProductRepr::fields)
    }

    fn sum_repr(&self, ty: ValueTypeId) -> Option<SumReprId> {
        let value_type = self.program.representations.value_type(ty)?;
        let Repr::Sum(sum) = self
            .program
            .representations
            .repr(value_type.repr())
            .copied()?
        else {
            return None;
        };
        self.program.representations.sum(sum).map(|_| sum)
    }

    fn list_element(&self, ty: ValueTypeId) -> Option<ValueTypeId> {
        let value_type = self.program.representations.value_type(ty)?;
        if value_type.kind() != ValueTypeKind::Direct
            || self.program.representations.repr(value_type.repr()) != Some(&Repr::ManagedPointer)
        {
            return None;
        }
        let Type::List(element) = value_type.semantic() else {
            return None;
        };
        self.program.representations.type_id(element)
    }

    fn task_list_output(&self, ty: ValueTypeId) -> Option<ValueTypeId> {
        let element = self.list_element(ty)?;
        let element_type = self.program.representations.value_type(element)?;
        let Type::Task(output) = element_type.semantic() else {
            return None;
        };
        let output = self.program.representations.type_id(output)?;
        self.canonical_task_handle(element, output)
            .then_some(output)
    }

    fn text_map_value(&self, ty: ValueTypeId) -> Option<ValueTypeId> {
        let value_type = self.program.representations.value_type(ty)?;
        if value_type.kind() != ValueTypeKind::ManagedTextMap
            || self.program.representations.repr(value_type.repr()) != Some(&Repr::ManagedPointer)
        {
            return None;
        }
        let Type::Nominal(identity, arguments) = value_type.semantic() else {
            return None;
        };
        if Some(*identity) != self.program.canonical_types.text_map {
            return None;
        }
        let [value] = arguments.as_slice() else {
            return None;
        };
        self.program.representations.type_id(value)
    }

    fn text_map_entry(&self, ty: ValueTypeId) -> Option<ValueTypeId> {
        let value = self.text_map_value(ty)?;
        let value = self.program.representations.value_type(value)?;
        self.program
            .representations
            .type_id(&Type::Tuple(vec![Type::Text, value.semantic().clone()]))
    }

    fn text_map_bulk_result(&self, entries: ValueTypeId) -> Option<ValueTypeId> {
        let entry = self.list_element(entries)?;
        let entry_type = self.program.representations.value_type(entry)?;
        let Type::Tuple(elements) = entry_type.semantic() else {
            return None;
        };
        let [semantic_key, semantic_value] = elements.as_slice() else {
            return None;
        };
        if semantic_key != &Type::Text {
            return None;
        }
        let fields = self.product_fields(entry)?;
        let [key, value] = fields else {
            return None;
        };
        let text = self.scalar_type(&Type::Text)?;
        let canonical_value = self.program.representations.type_id(semantic_value)?;
        if *key != text || *value != canonical_value {
            return None;
        }
        let value_semantic = semantic_value.clone();
        let map_semantic =
            Type::Nominal(self.program.canonical_types.text_map?, vec![value_semantic]);
        let map = self.program.representations.type_id(&map_semantic)?;
        self.text_map_value(map)?;
        let result_semantic = Type::Nominal(
            self.program.canonical_types.result?,
            vec![map_semantic, Type::Text],
        );
        let result = self.program.representations.type_id(&result_semantic)?;
        let sum = self.sum_repr(result)?;
        let variants = self.program.representations.sum(sum)?.variants();
        (variants.len() == 2 && variants[0].fields() == [map] && variants[1].fields() == [text])
            .then_some(result)
    }

    fn option_element(&self, ty: ValueTypeId) -> Option<ValueTypeId> {
        let value_type = self.program.representations.value_type(ty)?;
        let Type::Nominal(identity, arguments) = value_type.semantic() else {
            return None;
        };
        if Some(*identity) != self.program.canonical_types.option {
            return None;
        }
        let [element] = arguments.as_slice() else {
            return None;
        };
        let element = self.program.representations.type_id(element)?;
        let sum = self.sum_repr(ty)?;
        let variants = self.program.representations.sum(sum)?.variants();
        (variants.len() == 2
            && variants[0].fields().is_empty()
            && variants[1].fields() == [element])
        .then_some(element)
    }

    fn sum_variant_field_count(&self, sum: SumReprId, variant: usize) -> Option<usize> {
        self.program
            .representations
            .sum(sum)?
            .variants()
            .get(variant)
            .map(|variant| variant.fields().len())
    }

    fn sum_variant_field(
        &self,
        sum: SumReprId,
        variant: usize,
        field: usize,
    ) -> Option<ValueTypeId> {
        self.program
            .representations
            .sum(sum)?
            .variants()
            .get(variant)?
            .fields()
            .get(field)
            .copied()
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the UTF-8 unit opcode carries its complete closed-result identity"
    )]
    fn validate_text_from_utf8_units_instruction(
        &mut self,
        function: &Function,
        instruction: &Instruction,
        units: ValueId,
        ok_variant: u32,
        error_variant: u32,
        invalid_utf8_variant: u32,
        path: &str,
    ) {
        let units_ty = function.value(units).map(Value::ty);
        let integer = self.scalar_type(&Type::Int);
        let canonical_list = self
            .program
            .representations
            .type_id(&Type::List(Box::new(Type::Int)));
        let exact_list = match (units_ty, canonical_list, integer) {
            (Some(units), Some(list), Some(integer)) => {
                units == list && self.list_element(units) == Some(integer)
            }
            _ => false,
        };
        if !exact_list {
            self.error(
                ValidationCode::TypeMismatch,
                format!("{path}.units"),
                "Text UTF-8 construction requires canonical List[Int]",
            );
        }
        self.validate_decode_text_result(
            function,
            instruction,
            ok_variant,
            error_variant,
            invalid_utf8_variant,
            path,
        );
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the Path opcode carries its complete closed-result identity"
    )]
    #[expect(
        clippy::too_many_lines,
        reason = "canonical Path, PathError, and nested Result shapes are checked atomically"
    )]
    fn validate_path_result(
        &mut self,
        function: &Function,
        instruction: &Instruction,
        ok_variant: u32,
        error_variant: u32,
        path_error_variant: u32,
        expected_path_error_variant: u32,
        path: &str,
    ) {
        self.require_results(function, instruction, &[None], path);

        let canonical_path = self.canonical_path_type().map(|(path, _)| path);
        if canonical_path.is_none() {
            self.error(
                ValidationCode::TypeMismatch,
                format!("{path}.result[0]"),
                "Path operation requires the cataloged canonical Path as one managed Text field",
            );
        }
        let Some((path_id, error_id, result_id)) = self
            .program
            .canonical_types
            .path
            .zip(self.program.canonical_types.path_error)
            .zip(self.program.canonical_types.result)
            .map(|((path, error), result)| (path, error, result))
        else {
            self.error(
                ValidationCode::TypeMismatch,
                format!("{path}.result[0]"),
                "Path operation requires cataloged Path, PathError, and Result identities",
            );
            return;
        };
        let path_semantic = Type::Nominal(path_id, Vec::new());
        let error_semantic = Type::Nominal(error_id, Vec::new());
        let result_semantic = Type::Nominal(result_id, vec![path_semantic, error_semantic.clone()]);
        let Some(result_ty) = instruction
            .results
            .first()
            .and_then(|result| function.value(*result))
            .map(Value::ty)
        else {
            return;
        };
        if self
            .program
            .representations
            .value_type(result_ty)
            .is_none_or(|value_type| value_type.semantic() != &result_semantic)
            || self.scalar_type(&result_semantic) != Some(result_ty)
        {
            self.error(
                ValidationCode::TypeMismatch,
                format!("{path}.result[0]"),
                "Path operation result must be the cataloged canonical Result[Path, PathError]",
            );
        }

        let Some(result_sum) = self.sum_repr(result_ty) else {
            self.error(
                ValidationCode::TypeMismatch,
                format!("{path}.result[0]"),
                "Path operation result must use a closed sum representation",
            );
            return;
        };
        if (ok_variant, error_variant) != (0, 1) {
            self.error(
                ValidationCode::InstructionShape,
                format!("{path}.variants"),
                "Path operation requires canonical Ok=0 and Err=1 variants",
            );
        }
        let variants = self
            .program
            .representations
            .sum(result_sum)
            .map(crate::SumRepr::variants)
            .unwrap_or_default();
        if variants.len() != 2 {
            self.error(
                ValidationCode::InstructionShape,
                format!("{path}.result[0]"),
                "Path Result must contain exactly two variants",
            );
            return;
        }
        if canonical_path.is_none_or(|path| variants[0].fields() != [path]) {
            self.error(
                ValidationCode::InstructionShape,
                format!("{path}.ok_variant"),
                "Path success variant must carry exactly canonical Path",
            );
        }

        let error_ty = variants[1].fields().first().copied();
        if variants[1].fields().len() != 1
            || error_ty.is_none_or(|error_ty| {
                self.program
                    .representations
                    .value_type(error_ty)
                    .is_none_or(|value_type| {
                        value_type.semantic() != &error_semantic
                            || value_type.kind() != ValueTypeKind::Direct
                    })
                    || self.scalar_type(&error_semantic) != Some(error_ty)
            })
        {
            self.error(
                ValidationCode::InstructionShape,
                format!("{path}.error_variant"),
                "Path error variant must carry exactly canonical direct PathError",
            );
            return;
        }
        let Some(error_sum) = error_ty.and_then(|error_ty| self.sum_repr(error_ty)) else {
            self.error(
                ValidationCode::TypeMismatch,
                format!("{path}.error_variant"),
                "PathError must use a closed sum representation",
            );
            return;
        };
        let exact_error = self
            .program
            .representations
            .sum(error_sum)
            .is_some_and(|sum| {
                sum.variants().len() == 2
                    && sum
                        .variants()
                        .iter()
                        .all(|variant| variant.fields().is_empty())
            });
        if !exact_error {
            self.error(
                ValidationCode::InstructionShape,
                format!("{path}.error_variant"),
                "PathError must contain only ContainsNul=0 and AbsoluteJoin=1 without payload",
            );
        }
        if path_error_variant != expected_path_error_variant {
            let expected = if expected_path_error_variant == 0 {
                "ContainsNul=0"
            } else {
                "AbsoluteJoin=1"
            };
            self.error(
                ValidationCode::InstructionShape,
                format!("{path}.path_error_variant"),
                format!("Path operation requires canonical {expected}"),
            );
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the Bytes decode opcode carries its complete closed-result identity"
    )]
    fn validate_bytes_decode_utf8_instruction(
        &mut self,
        function: &Function,
        instruction: &Instruction,
        bytes_value: ValueId,
        ok_variant: u32,
        error_variant: u32,
        invalid_utf8_variant: u32,
        path: &str,
    ) {
        let bytes = self.managed_bytes_type();
        if bytes.is_none() {
            self.error(
                ValidationCode::TypeMismatch,
                format!("{path}.bytes"),
                "UTF-8 decoding requires cataloged canonical managed Bytes",
            );
        }
        self.require_known_value_type(
            function,
            bytes_value,
            bytes,
            ValidationCode::TypeMismatch,
            format!("{path}.bytes"),
        );
        self.validate_decode_text_result(
            function,
            instruction,
            ok_variant,
            error_variant,
            invalid_utf8_variant,
            path,
        );
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the complete nested Result and DecodeTextError shape is checked atomically"
    )]
    fn validate_decode_text_result(
        &mut self,
        function: &Function,
        instruction: &Instruction,
        ok_variant: u32,
        error_variant: u32,
        invalid_utf8_variant: u32,
        path: &str,
    ) {
        self.require_results(function, instruction, &[None], path);

        let text = self.scalar_type(&Type::Text);
        let managed_text = text.filter(|text| {
            self.program
                .representations
                .value_type(*text)
                .is_some_and(|value_type| {
                    self.program.representations.repr(value_type.repr())
                        == Some(&Repr::ManagedPointer)
                })
        });
        if managed_text.is_none() {
            self.error(
                ValidationCode::TypeMismatch,
                format!("{path}.result[0]"),
                "UTF-8 decoding requires the canonical managed Text representation",
            );
        }

        let Some(result_ty) = instruction
            .results
            .first()
            .and_then(|result| function.value(*result))
            .map(Value::ty)
        else {
            return;
        };
        let Some((error_id, result_id)) = self
            .program
            .canonical_types
            .decode_text_error
            .zip(self.program.canonical_types.result)
        else {
            self.error(
                ValidationCode::TypeMismatch,
                format!("{path}.result[0]"),
                "UTF-8 decoding requires cataloged DecodeTextError and Result identities",
            );
            return;
        };
        let error_semantic = Type::Nominal(error_id, Vec::new());
        let result_semantic = Type::Nominal(result_id, vec![Type::Text, error_semantic.clone()]);
        if self
            .program
            .representations
            .value_type(result_ty)
            .is_none_or(|value_type| value_type.semantic() != &result_semantic)
            || self.scalar_type(&result_semantic) != Some(result_ty)
        {
            self.error(
                ValidationCode::TypeMismatch,
                format!("{path}.result[0]"),
                "UTF-8 decoding result must be the cataloged canonical Result[Text, DecodeTextError]",
            );
        }

        let Some(result_sum) = self.sum_repr(result_ty) else {
            self.error(
                ValidationCode::TypeMismatch,
                format!("{path}.result[0]"),
                "UTF-8 decoding result must use a closed sum representation",
            );
            return;
        };
        if (ok_variant, error_variant) != (0, 1) {
            self.error(
                ValidationCode::InstructionShape,
                format!("{path}.variants"),
                "UTF-8 decoding requires canonical Ok=0 and Err=1 variants",
            );
        }
        let variants = self
            .program
            .representations
            .sum(result_sum)
            .map(crate::SumRepr::variants)
            .unwrap_or_default();
        if variants.len() != 2 {
            self.error(
                ValidationCode::InstructionShape,
                format!("{path}.result[0]"),
                "UTF-8 decoding Result must contain exactly two variants",
            );
            return;
        }
        if managed_text.is_none_or(|text| variants[0].fields() != [text]) {
            self.error(
                ValidationCode::InstructionShape,
                format!("{path}.ok_variant"),
                "UTF-8 decoding success variant must carry exactly managed Text",
            );
        }
        let error_ty = variants[1].fields().first().copied();
        if variants[1].fields().len() != 1
            || error_ty.is_none_or(|error_ty| {
                self.program
                    .representations
                    .value_type(error_ty)
                    .is_none_or(|value_type| {
                        value_type.semantic() != &error_semantic
                            || value_type.kind() != ValueTypeKind::Direct
                    })
                    || self.scalar_type(&error_semantic) != Some(error_ty)
            })
        {
            self.error(
                ValidationCode::InstructionShape,
                format!("{path}.error_variant"),
                "UTF-8 decoding error variant must carry exactly canonical direct DecodeTextError",
            );
            return;
        }
        let Some(error_sum) = error_ty.and_then(|error_ty| self.sum_repr(error_ty)) else {
            self.error(
                ValidationCode::TypeMismatch,
                format!("{path}.error_variant"),
                "DecodeTextError must use a closed sum representation",
            );
            return;
        };
        let invalid_utf8 = usize::try_from(invalid_utf8_variant).ok();
        let exact_error = self
            .program
            .representations
            .sum(error_sum)
            .is_some_and(|sum| {
                sum.variants().len() == 1
                    && invalid_utf8 == Some(0)
                    && sum.variants()[0].fields().is_empty()
            });
        if !exact_error {
            self.error(
                ValidationCode::InstructionShape,
                format!("{path}.invalid_utf8_variant"),
                "DecodeTextError must contain only canonical InvalidUtf8=0 without payload",
            );
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the JSON formatter's closed input, Result, and JsonError identities are validated at one boundary"
    )]
    #[expect(
        clippy::too_many_lines,
        reason = "the recursive Json and nested Result[Text, JsonError] shapes are checked atomically"
    )]
    fn validate_json_format_instruction(
        &mut self,
        function: &Function,
        instruction: &Instruction,
        json: ValueId,
        ok_variant: u32,
        error_variant: u32,
        depth_limit_variant: u32,
        non_finite_number_variant: u32,
        path: &str,
    ) {
        self.require_results(function, instruction, &[None], path);

        let Some(json_ty) = function.value(json).map(Value::ty) else {
            return;
        };
        let Some((json_id, text_map_id, json_error_id, result_id)) = self
            .program
            .canonical_types
            .json
            .zip(self.program.canonical_types.text_map)
            .zip(self.program.canonical_types.json_error)
            .zip(self.program.canonical_types.result)
            .map(|(((json, text_map), json_error), result)| (json, text_map, json_error, result))
        else {
            self.error(
                ValidationCode::TypeMismatch,
                format!("{path}.json"),
                "JSON formatting requires cataloged Json, TextMap, JsonError, and Result identities",
            );
            return;
        };
        let canonical_json_semantic = Type::Nominal(json_id, Vec::new());
        let json_semantic = self
            .program
            .representations
            .value_type(json_ty)
            .map(crate::ValueType::semantic);
        let Some(json_sum) = self.sum_repr(json_ty) else {
            self.error(
                ValidationCode::TypeMismatch,
                format!("{path}.json"),
                "JSON formatting requires the canonical recursive Json sum",
            );
            return;
        };
        if json_semantic != Some(&canonical_json_semantic)
            || self.scalar_type(&canonical_json_semantic) != Some(json_ty)
        {
            self.error(
                ValidationCode::TypeMismatch,
                format!("{path}.json"),
                "JSON formatting requires the cataloged canonical Json type",
            );
        }
        let variants = self
            .program
            .representations
            .sum(json_sum)
            .map_or(0, |sum| sum.variants().len());
        let boolean = self.scalar_type(&Type::Bool);
        let float = self.scalar_type(&Type::Float);
        let text = self.scalar_type(&Type::Text);
        let array_semantic = Type::List(Box::new(canonical_json_semantic.clone()));
        let object_semantic = Type::Nominal(text_map_id, vec![canonical_json_semantic.clone()]);
        let exact_scalar_variant = |validator: &Self, variant: usize, expected| {
            validator.sum_variant_field_count(json_sum, variant) == Some(1)
                && validator.sum_variant_field(json_sum, variant, 0) == expected
        };
        let array_ty = self.sum_variant_field(json_sum, 4, 0);
        let object_ty = self.sum_variant_field(json_sum, 5, 0);
        let canonical_json = variants == 6
            && self.sum_variant_field_count(json_sum, 0) == Some(0)
            && exact_scalar_variant(self, 1, boolean)
            && exact_scalar_variant(self, 2, float)
            && exact_scalar_variant(self, 3, text)
            && self.sum_variant_field_count(json_sum, 4) == Some(1)
            && array_ty == self.scalar_type(&array_semantic)
            && array_ty.and_then(|ty| self.list_element(ty)) == Some(json_ty)
            && self.sum_variant_field_count(json_sum, 5) == Some(1)
            && object_ty == self.scalar_type(&object_semantic)
            && object_ty.and_then(|ty| self.text_map_value(ty)) == Some(json_ty);
        if !canonical_json {
            self.error(
                ValidationCode::TypeMismatch,
                format!("{path}.json"),
                "Json must contain Null, Bool(Bool), Number(Float), Text(Text), Array(List[Json]), and Object(TextMap[Json]) in canonical order",
            );
        }

        let Some(result_ty) = instruction
            .results
            .first()
            .and_then(|result| function.value(*result))
            .map(Value::ty)
        else {
            return;
        };
        let result_semantic = self
            .program
            .representations
            .value_type(result_ty)
            .map(|value_type| value_type.semantic().clone());
        let error_semantic = Type::Nominal(json_error_id, Vec::new());
        let canonical_result_semantic =
            Type::Nominal(result_id, vec![Type::Text, error_semantic.clone()]);
        if result_semantic.as_ref() != Some(&canonical_result_semantic)
            || self.scalar_type(&canonical_result_semantic) != Some(result_ty)
        {
            self.error(
                ValidationCode::TypeMismatch,
                format!("{path}.result[0]"),
                "JSON formatting result must be the cataloged canonical Result[Text, JsonError]",
            );
        }

        let Some(result_sum) = self.sum_repr(result_ty) else {
            self.error(
                ValidationCode::TypeMismatch,
                format!("{path}.result[0]"),
                "JSON formatting result must use a closed sum representation",
            );
            return;
        };
        if self
            .program
            .representations
            .sum(result_sum)
            .map_or(0, |sum| sum.variants().len())
            != 2
        {
            self.error(
                ValidationCode::InstructionShape,
                format!("{path}.result[0]"),
                "JSON formatting Result must contain exactly two variants",
            );
        }
        if (ok_variant, error_variant) != (0, 1) {
            self.error(
                ValidationCode::InstructionShape,
                format!("{path}.variants"),
                "JSON formatting Result requires canonical Ok=0 and Err=1 variants",
            );
        }
        let ok = usize::try_from(ok_variant).ok();
        if ok.map(|variant| {
            (
                self.sum_variant_field_count(result_sum, variant),
                self.sum_variant_field(result_sum, variant, 0),
            )
        }) != Some((Some(1), text))
        {
            self.error(
                ValidationCode::InstructionShape,
                format!("{path}.ok_variant"),
                "JSON formatting success variant must exist and carry exactly Text",
            );
        }
        let error = usize::try_from(error_variant).ok();
        let error_ty = error.and_then(|variant| {
            (self.sum_variant_field_count(result_sum, variant) == Some(1))
                .then(|| self.sum_variant_field(result_sum, variant, 0))
                .flatten()
        });
        let Some(error_ty) = error_ty else {
            self.error(
                ValidationCode::InstructionShape,
                format!("{path}.error_variant"),
                "JSON formatting error variant must exist and carry exactly one JsonError",
            );
            return;
        };
        if self
            .program
            .representations
            .value_type(error_ty)
            .is_none_or(|value_type| value_type.semantic() != &error_semantic)
            || self.scalar_type(&error_semantic) != Some(error_ty)
        {
            self.error(
                ValidationCode::TypeMismatch,
                format!("{path}.error_variant"),
                "JSON formatting error payload must match the Result error argument exactly",
            );
        }
        let Some(error_sum) = self.sum_repr(error_ty) else {
            self.error(
                ValidationCode::TypeMismatch,
                format!("{path}.error_variant"),
                "JsonError payload must use a closed sum representation",
            );
            return;
        };
        if self
            .program
            .representations
            .sum(error_sum)
            .map_or(0, |sum| sum.variants().len())
            != 4
        {
            self.error(
                ValidationCode::InstructionShape,
                format!("{path}.error_variant"),
                "JsonError must contain exactly four variants",
            );
        }
        if (depth_limit_variant, non_finite_number_variant) != (2, 3) {
            self.error(
                ValidationCode::InstructionShape,
                format!("{path}.error_variants"),
                "JSON formatting requires canonical JsonError DepthLimit=2 and NonFiniteNumber=3 variants",
            );
        }
        for (name, variant) in [
            ("depth_limit_variant", depth_limit_variant),
            ("non_finite_number_variant", non_finite_number_variant),
        ] {
            if usize::try_from(variant)
                .ok()
                .and_then(|variant| self.sum_variant_field_count(error_sum, variant))
                != Some(0)
            {
                self.error(
                    ValidationCode::InstructionShape,
                    format!("{path}.{name}"),
                    "JsonError status variant must exist and carry no payload",
                );
            }
        }
        let integer = self.scalar_type(&Type::Int);
        for variant in 0..4 {
            let mapped_status = usize::try_from(depth_limit_variant).ok() == Some(variant)
                || usize::try_from(non_finite_number_variant).ok() == Some(variant);
            if !mapped_status
                && (self.sum_variant_field_count(error_sum, variant) != Some(1)
                    || self.sum_variant_field(error_sum, variant, 0) != integer)
            {
                self.error(
                    ValidationCode::InstructionShape,
                    format!("{path}.error_variant[{variant}]"),
                    "each non-status JsonError variant must carry exactly one Int offset",
                );
            }
        }
    }

    fn signature_writeback_types(function: &Function) -> Vec<ValueTypeId> {
        function
            .signature
            .inout_params()
            .iter()
            .filter_map(|parameter| {
                usize::try_from(*parameter)
                    .ok()
                    .and_then(|index| function.signature.params().get(index))
                    .copied()
            })
            .collect()
    }

    fn require_type(&mut self, ty: ValueTypeId, path: String) {
        if self.program.representations.value_type(ty).is_none() {
            self.error(
                ValidationCode::InvalidTypeReference,
                path,
                format!("value type {ty} does not exist"),
            );
        }
    }

    fn require_inhabited_type(&mut self, ty: ValueTypeId, path: String) {
        let is_uninhabited = self
            .program
            .representations
            .value_type(ty)
            .and_then(|value_type| {
                self.program
                    .representations
                    .repr(value_type.repr())
                    .copied()
            })
            == Some(Repr::Uninhabited);
        if is_uninhabited {
            self.error(
                ValidationCode::UninhabitedValue,
                path,
                "the direct foundation cannot materialize an uninhabited SSA value; lower Never-producing operations as terminators",
            );
        }
    }

    fn error(&mut self, code: ValidationCode, path: impl Into<String>, message: impl Into<String>) {
        self.errors.push(ValidationError {
            code,
            path: path.into(),
            message: message.into(),
        });
    }
}

fn compute_program_fault_states(program: &Program) -> Vec<Vec<FaultStateSet>> {
    program
        .functions
        .iter()
        .map(|function| {
            let mut edges = vec![Vec::new(); function.blocks.len()];
            for (source, block) in function.blocks.iter().enumerate() {
                let Some(terminator) = block.terminator.as_ref() else {
                    continue;
                };
                for edge in terminator.control_flow_edges() {
                    if edge.block.owner() == function.id
                        && edge.block.index() < function.blocks.len()
                    {
                        edges[source].push(FaultEdge {
                            target: edge.block.index(),
                            activates_fault: edge.activates_fault,
                        });
                    }
                }
            }
            function.entry.map_or_else(
                || vec![FaultStateSet::NONE; function.blocks.len()],
                |entry| {
                    if entry.owner() == function.id && entry.index() < function.blocks.len() {
                        compute_fault_states(entry.index(), &edges)
                    } else {
                        vec![FaultStateSet::NONE; function.blocks.len()]
                    }
                },
            )
        })
        .collect()
}

fn instruction_direct_effects(kind: &InstructionKind) -> Effects {
    let mut effects = Effects::NONE;
    if matches!(
        kind,
        InstructionKind::TextConcat { .. }
            | InstructionKind::TextGet { .. }
            | InstructionKind::TextFromUtf8Units { .. }
            | InstructionKind::ProcessArgumentAt { .. }
            | InstructionKind::ProcessEnvironment { .. }
            | InstructionKind::PathJoin { .. }
            | InstructionKind::BytesAppend { .. }
            | InstructionKind::BytesDecodeUtf8 { .. }
            | InstructionKind::FloatFormat { .. }
            | InstructionKind::JsonFormat { .. }
            | InstructionKind::ListAppend { .. }
            | InstructionKind::ListAppendUnique { .. }
            | InstructionKind::TextMapInsert { .. }
            | InstructionKind::TextMapConstructEntries { .. }
            | InstructionKind::TextMapRemove { .. }
            | InstructionKind::DynConstruct { .. }
            | InstructionKind::TaskOutcomeTake { .. }
    ) || matches!(kind, InstructionKind::ListConstruct { elements } if !elements.is_empty())
    {
        effects = effects.union(Effects::MAY_COLLECT);
    }
    if matches!(
        kind,
        InstructionKind::TaskCreate { .. }
            | InstructionKind::IoTaskCreate { .. }
            | InstructionKind::ResourceClose { .. }
            | InstructionKind::TaskJoin { .. }
            | InstructionKind::TaskJoinList { .. }
            | InstructionKind::TaskOutcomeTake { .. }
    ) {
        effects = effects.union(Effects::NEEDS_EXECUTOR);
    }
    effects
}

/// Computes the least transitive function effects from operations and
/// synchronous call edges. `TaskCreate` is an ownership transfer into a new
/// coroutine, not execution of the child body, so child fault, collection, and
/// suspension capabilities never propagate into the creator. Operations in
/// active cleanup can only preserve the primary fault or suppress a secondary
/// one, so those paths strip only `MAY_FAULT`; runtime, collection, executor,
/// and suspension capabilities still propagate across synchronous calls.
fn compute_exact_effects(program: &Program, fault_states: &[Vec<FaultStateSet>]) -> Vec<Effects> {
    let function_count = program.functions.len();
    let mut reverse_calls = vec![Vec::new(); function_count];
    let mut effects = vec![Effects::NONE; function_count];

    for (caller, function) in program.functions.iter().enumerate() {
        if function.coroutine().is_some() {
            effects[caller] = effects[caller].union(Effects::NEEDS_EXECUTOR);
        }
        for (block_index, block) in function.blocks.iter().enumerate() {
            let state = fault_states
                .get(caller)
                .and_then(|states| states.get(block_index))
                .copied()
                .unwrap_or(FaultStateSet::NONE);
            if state == FaultStateSet::NONE {
                continue;
            }
            let propagates_fault = state.contains(FaultStateSet::INACTIVE);

            for instruction_id in block.instructions.iter().copied() {
                let Some(instruction) = function.instruction(instruction_id) else {
                    continue;
                };
                effects[caller] =
                    effects[caller].union(instruction_direct_effects(instruction.kind()));
                if let InstructionKind::DirectCall { callee, .. } = instruction.kind()
                    && let Some(callee) = canonical_function_index(program, *callee)
                {
                    reverse_calls[callee].push(EffectCaller {
                        caller,
                        propagates_fault,
                    });
                }
            }

            let Some(terminator) = block.terminator.as_ref() else {
                continue;
            };
            match terminator.kind() {
                TerminatorKind::CheckedIntNegate { .. }
                | TerminatorKind::CheckedIntBinary { .. }
                | TerminatorKind::LogWrite { .. }
                | TerminatorKind::StdoutWrite { .. }
                | TerminatorKind::Assert { .. }
                | TerminatorKind::Fault { .. }
                    if propagates_fault =>
                {
                    effects[caller] = effects[caller].union(Effects::MAY_FAULT);
                }
                TerminatorKind::TaskSleep { .. } => {
                    effects[caller] = effects[caller].union(Effects::NEEDS_EXECUTOR);
                    if propagates_fault {
                        effects[caller] = effects[caller].union(Effects::MAY_FAULT);
                    }
                }
                TerminatorKind::Invoke { callee, .. } => {
                    if let Some(callee) = canonical_function_index(program, *callee) {
                        reverse_calls[callee].push(EffectCaller {
                            caller,
                            propagates_fault,
                        });
                    }
                }
                TerminatorKind::AwaitTasks { .. } => {
                    effects[caller] = effects[caller]
                        .union(Effects::MAY_FAULT)
                        .union(Effects::MAY_SUSPEND);
                }
                TerminatorKind::CheckedIntNegate { .. }
                | TerminatorKind::CheckedIntBinary { .. }
                | TerminatorKind::LogWrite { .. }
                | TerminatorKind::StdoutWrite { .. }
                | TerminatorKind::Assert { .. }
                | TerminatorKind::Fault { .. }
                | TerminatorKind::Jump(_)
                | TerminatorKind::Branch { .. }
                | TerminatorKind::SumSwitch { .. }
                | TerminatorKind::DynSwitch { .. }
                | TerminatorKind::Return(_)
                | TerminatorKind::TaskCancelled
                // Propagation cannot seed the least effect fixed point.
                | TerminatorKind::ResumeFault => {}
            }
        }
    }

    propagate_effects(effects, &reverse_calls)
}

fn propagate_effects(
    mut effects: Vec<Effects>,
    reverse_calls: &[Vec<EffectCaller>],
) -> Vec<Effects> {
    for effect in &mut effects {
        *effect = effect.with_implications();
    }
    let mut pending = effects
        .iter()
        .copied()
        .enumerate()
        .filter_map(|(index, effect)| (!effect.is_empty()).then_some(index))
        .collect::<VecDeque<_>>();
    while let Some(callee) = pending.pop_front() {
        for edge in reverse_calls[callee].iter().copied() {
            let propagated = if edge.propagates_fault {
                effects[callee]
            } else {
                effects[callee].without(Effects::MAY_FAULT)
            };
            let joined = effects[edge.caller].union(propagated).with_implications();
            if joined != effects[edge.caller] {
                effects[edge.caller] = joined;
                pending.push_back(edge.caller);
            }
        }
    }

    effects
}

fn canonical_function_index(program: &Program, function: InstanceId) -> Option<usize> {
    program
        .instances
        .key(function)
        .and_then(|_| program.functions.get(function.index()))
        .filter(|candidate| candidate.id == function)
        .map(|_| function.index())
}

#[derive(Clone, Copy, Debug)]
struct FaultEdge {
    target: usize,
    activates_fault: bool,
}

#[derive(Clone, Copy, Debug)]
struct EffectCaller {
    caller: usize,
    propagates_fault: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FaultStateSet(u8);

impl FaultStateSet {
    const NONE: Self = Self(0);
    const INACTIVE: Self = Self(1);
    const ACTIVE: Self = Self(2);
    const BOTH: Self = Self(3);

    const fn contains(self, state: Self) -> bool {
        self.0 & state.0 == state.0
    }

    fn insert(&mut self, state: Self) -> bool {
        let previous = self.0;
        self.0 |= state.0;
        self.0 != previous
    }
}

fn compute_fault_states(entry: usize, edges: &[Vec<FaultEdge>]) -> Vec<FaultStateSet> {
    let mut states = vec![FaultStateSet::NONE; edges.len()];
    let Some(entry_state) = states.get_mut(entry) else {
        return states;
    };
    *entry_state = FaultStateSet::INACTIVE;
    let mut pending = VecDeque::from([entry]);
    while let Some(source) = pending.pop_front() {
        let Some(source_state) = states.get(source).copied() else {
            continue;
        };
        let Some(outgoing) = edges.get(source) else {
            continue;
        };
        for edge in outgoing {
            // An unwind edge activates the first fault or suppresses a later
            // cleanup fault. In either case the destination carries exactly
            // the same active-state obligation: cleanup must continue and
            // eventually resume the primary fault.
            let incoming = if edge.activates_fault {
                FaultStateSet::ACTIVE
            } else {
                source_state
            };
            let Some(target_state) = states.get_mut(edge.target) else {
                continue;
            };
            if target_state.insert(incoming) {
                pending.push_back(edge.target);
            }
        }
    }
    states
}

#[derive(Clone, Copy, Debug)]
struct CancellationEdge {
    target: usize,
    activates_cancellation: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CancellationStateSet(u8);

impl CancellationStateSet {
    const NONE: Self = Self(0);
    const INACTIVE: Self = Self(1);
    const ACTIVE: Self = Self(2);
    const BOTH: Self = Self(3);

    const fn contains(self, state: Self) -> bool {
        self.0 & state.0 == state.0
    }

    fn insert(&mut self, state: Self) -> bool {
        let previous = self.0;
        self.0 |= state.0;
        self.0 != previous
    }
}

fn compute_cancellation_states(function: &Function) -> Vec<CancellationStateSet> {
    let mut edges = vec![Vec::new(); function.blocks.len()];
    for (source, block) in function.blocks.iter().enumerate() {
        let Some(terminator) = block.terminator.as_ref() else {
            continue;
        };
        match terminator.kind() {
            TerminatorKind::AwaitTasks {
                normal,
                fault,
                cancel,
                ..
            } => {
                for (target, activates_cancellation) in [
                    (normal.block, false),
                    (fault.block, false),
                    (cancel.block, true),
                ] {
                    if target.owner() == function.id && target.index() < function.blocks.len() {
                        edges[source].push(CancellationEdge {
                            target: target.index(),
                            activates_cancellation,
                        });
                    }
                }
            }
            _ => {
                for edge in terminator.control_flow_edges() {
                    if edge.block.owner() == function.id
                        && edge.block.index() < function.blocks.len()
                    {
                        edges[source].push(CancellationEdge {
                            target: edge.block.index(),
                            activates_cancellation: false,
                        });
                    }
                }
            }
        }
    }

    let mut states = vec![CancellationStateSet::NONE; function.blocks.len()];
    let Some(entry) = function
        .entry
        .filter(|entry| entry.owner() == function.id && entry.index() < function.blocks.len())
    else {
        return states;
    };
    states[entry.index()] = CancellationStateSet::INACTIVE;
    let mut pending = VecDeque::from([entry.index()]);
    while let Some(source) = pending.pop_front() {
        let source_state = states[source];
        for edge in &edges[source] {
            let incoming = if edge.activates_cancellation {
                CancellationStateSet::ACTIVE
            } else {
                source_state
            };
            if states[edge.target].insert(incoming) {
                pending.push_back(edge.target);
            }
        }
    }
    states
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TaskAvailability(u8);

impl TaskAvailability {
    const NONE: Self = Self(0);
    const AVAILABLE: Self = Self(1);
    const CONSUMED: Self = Self(2);

    const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum TaskOwnershipSite {
    Instruction(InstructionId),
    Terminator(BlockId),
}

#[derive(Clone, Copy)]
struct TaskOwnershipIssue {
    site: TaskOwnershipSite,
    value: ValueId,
    message: &'static str,
}

struct TaskOwnershipEdge {
    target: usize,
    states: Vec<TaskAvailability>,
}

struct TaskOwnershipTransfer {
    edges: Vec<TaskOwnershipEdge>,
    issues: Vec<TaskOwnershipIssue>,
}

fn consume_task_handle(
    value: ValueId,
    site: TaskOwnershipSite,
    states: &mut [TaskAvailability],
    task_values: &[bool],
    collect_issues: bool,
    issues: &mut Vec<TaskOwnershipIssue>,
) {
    if !task_values.get(value.index()).copied().unwrap_or(false) {
        return;
    }
    let Some(state) = states.get_mut(value.index()) else {
        return;
    };
    if collect_issues && *state != TaskAvailability::AVAILABLE {
        issues.push(TaskOwnershipIssue {
            site,
            value,
            message: "an affine Task carrier is consumed more than once or is unavailable on an incoming control-flow path",
        });
    }
    *state = TaskAvailability::CONSUMED;
}

fn borrow_task_carrier(
    value: ValueId,
    site: TaskOwnershipSite,
    states: &[TaskAvailability],
    task_values: &[bool],
    collect_issues: bool,
    issues: &mut Vec<TaskOwnershipIssue>,
) {
    if !task_values.get(value.index()).copied().unwrap_or(false) {
        return;
    }
    if collect_issues && states.get(value.index()).copied() != Some(TaskAvailability::AVAILABLE) {
        issues.push(TaskOwnershipIssue {
            site,
            value,
            message: "an affine Task carrier is borrowed after it was consumed or is unavailable on an incoming control-flow path",
        });
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the affine Task transfer keeps instruction uses, implicit results, and edge moves in one auditable fixed-point operation"
)]
fn transfer_task_ownership(
    function: &Function,
    block: &crate::Block,
    input: &[TaskAvailability],
    task_values: &[bool],
    collect_issues: bool,
) -> TaskOwnershipTransfer {
    let mut states = input.to_vec();
    let mut issues = Vec::new();
    for instruction_id in block.instructions().iter().copied() {
        let Some(instruction) = function.instruction(instruction_id) else {
            continue;
        };
        let site = TaskOwnershipSite::Instruction(instruction_id);
        match instruction.kind() {
            InstructionKind::ListLength { list } => borrow_task_carrier(
                *list,
                site,
                &states,
                task_values,
                collect_issues,
                &mut issues,
            ),
            InstructionKind::ProductExtract { aggregate, .. } => borrow_task_carrier(
                *aggregate,
                site,
                &states,
                task_values,
                collect_issues,
                &mut issues,
            ),
            kind => {
                for operand in kind.operands() {
                    consume_task_handle(
                        operand,
                        site,
                        &mut states,
                        task_values,
                        collect_issues,
                        &mut issues,
                    );
                }
            }
        }
        for result in instruction.results().iter().copied() {
            if task_values.get(result.index()).copied().unwrap_or(false) {
                states[result.index()] = TaskAvailability::AVAILABLE;
            }
        }
    }

    let Some(terminator) = block.terminator() else {
        return TaskOwnershipTransfer {
            edges: Vec::new(),
            issues,
        };
    };
    let site = TaskOwnershipSite::Terminator(block.id());
    match terminator.kind() {
        TerminatorKind::Invoke { arguments, .. } => {
            for argument in arguments.iter().copied() {
                consume_task_handle(
                    argument,
                    site,
                    &mut states,
                    task_values,
                    collect_issues,
                    &mut issues,
                );
            }
        }
        TerminatorKind::AwaitTasks { tasks, .. } => {
            for task in tasks.iter().copied() {
                consume_task_handle(
                    task,
                    site,
                    &mut states,
                    task_values,
                    collect_issues,
                    &mut issues,
                );
            }
        }
        TerminatorKind::Return(value) => consume_task_handle(
            *value,
            site,
            &mut states,
            task_values,
            collect_issues,
            &mut issues,
        ),
        TerminatorKind::SumSwitch { scrutinee, .. } => consume_task_handle(
            *scrutinee,
            site,
            &mut states,
            task_values,
            collect_issues,
            &mut issues,
        ),
        _ => {}
    }
    for writeback in terminator.writebacks().iter().copied() {
        consume_task_handle(
            writeback,
            site,
            &mut states,
            task_values,
            collect_issues,
            &mut issues,
        );
    }

    let mut edges = Vec::new();
    for (target, arguments) in forwarded_list_edges(terminator.kind()) {
        let Some(target_block) = function.block(target) else {
            continue;
        };
        let mut edge_states = states.clone();
        let implicit = target_block.params().len().saturating_sub(arguments.len());

        // Consume every source before defining any destination. This ordering
        // is essential for self-loop phis: forwarding one handle into two
        // parameters must not let the first parameter definition resurrect it
        // before the duplicate second move is checked.
        for argument in arguments.iter().copied() {
            consume_task_handle(
                argument,
                site,
                &mut edge_states,
                task_values,
                collect_issues,
                &mut issues,
            );
        }
        for parameter in target_block.params().iter().copied().take(implicit) {
            if task_values.get(parameter.index()).copied().unwrap_or(false) {
                edge_states[parameter.index()] = TaskAvailability::AVAILABLE;
            }
        }
        for parameter in target_block.params().iter().copied().skip(implicit) {
            if task_values.get(parameter.index()).copied().unwrap_or(false) {
                edge_states[parameter.index()] = TaskAvailability::AVAILABLE;
            }
        }
        edges.push(TaskOwnershipEdge {
            target: target.index(),
            states: edge_states,
        });
    }
    TaskOwnershipTransfer { edges, issues }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ListOwnership {
    Consumed,
    Shared,
    Unique,
}

impl ListOwnership {
    const fn meet(self, other: Self) -> Self {
        match (self, other) {
            (Self::Consumed, _) | (_, Self::Consumed) => Self::Consumed,
            (Self::Shared, _) | (_, Self::Shared) => Self::Shared,
            (Self::Unique, Self::Unique) => Self::Unique,
        }
    }
}

struct ListOwnershipEdge {
    target: usize,
    states: Vec<ListOwnership>,
}

#[derive(Clone, Copy)]
struct ListOwnershipIssue {
    instruction: InstructionId,
    value: ValueId,
    message: &'static str,
}

struct ListOwnershipTransfer {
    edges: Vec<ListOwnershipEdge>,
    issues: Vec<ListOwnershipIssue>,
}

fn apply_list_use(
    instruction: InstructionId,
    value: ValueId,
    state: &mut ListOwnership,
    shares: bool,
    collect_issues: bool,
    list_values: &[bool],
    issues: &mut Vec<ListOwnershipIssue>,
) {
    if !list_values.get(value.index()).copied().unwrap_or(false) {
        return;
    }
    if collect_issues && *state == ListOwnership::Consumed {
        issues.push(ListOwnershipIssue {
            instruction,
            value,
            message: "a consumed unique List value is used again",
        });
    }
    if shares {
        *state = ListOwnership::Shared;
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the ownership transfer keeps instruction sequencing and edge moves in one auditable fixed-point operation"
)]
fn transfer_list_ownership(
    function: &Function,
    block: &crate::Block,
    input: &[ListOwnership],
    list_values: &[bool],
    collect_issues: bool,
) -> ListOwnershipTransfer {
    let mut states = input.to_vec();
    let mut issues = Vec::new();
    for instruction_id in block.instructions().iter().copied() {
        let Some(instruction) = function.instruction(instruction_id) else {
            continue;
        };
        match instruction.kind() {
            InstructionKind::ListConstruct { elements } => {
                for element in elements.iter().copied() {
                    if let Some(state) = states.get_mut(element.index()) {
                        apply_list_use(
                            instruction_id,
                            element,
                            state,
                            true,
                            collect_issues,
                            list_values,
                            &mut issues,
                        );
                    }
                }
            }
            InstructionKind::ListAppend { list, value } => {
                if let Some(state) = states.get_mut(list.index()) {
                    apply_list_use(
                        instruction_id,
                        *list,
                        state,
                        false,
                        collect_issues,
                        list_values,
                        &mut issues,
                    );
                }
                if let Some(state) = states.get_mut(value.index()) {
                    apply_list_use(
                        instruction_id,
                        *value,
                        state,
                        true,
                        collect_issues,
                        list_values,
                        &mut issues,
                    );
                }
            }
            InstructionKind::ListAppendUnique { list, value } => {
                if let Some(state) = states.get_mut(list.index()) {
                    if collect_issues && *state != ListOwnership::Unique {
                        issues.push(ListOwnershipIssue {
                            instruction: instruction_id,
                            value: *list,
                            message: "list.append.unique receiver is not uniquely owned on every incoming edge",
                        });
                    }
                    *state = ListOwnership::Consumed;
                }
                if let Some(state) = states.get_mut(value.index()) {
                    apply_list_use(
                        instruction_id,
                        *value,
                        state,
                        true,
                        collect_issues,
                        list_values,
                        &mut issues,
                    );
                }
            }
            InstructionKind::ListLength { list } | InstructionKind::ListGet { list, .. } => {
                if let Some(state) = states.get_mut(list.index()) {
                    apply_list_use(
                        instruction_id,
                        *list,
                        state,
                        false,
                        collect_issues,
                        list_values,
                        &mut issues,
                    );
                }
            }
            kind => {
                for operand in kind.operands() {
                    if let Some(state) = states.get_mut(operand.index()) {
                        apply_list_use(
                            instruction_id,
                            operand,
                            state,
                            true,
                            collect_issues,
                            list_values,
                            &mut issues,
                        );
                    }
                }
            }
        }

        for result in instruction.results().iter().copied() {
            if !list_values.get(result.index()).copied().unwrap_or(false) {
                continue;
            }
            states[result.index()] = if matches!(
                instruction.kind(),
                InstructionKind::ListConstruct { .. }
                    | InstructionKind::ListAppend { .. }
                    | InstructionKind::ListAppendUnique { .. }
            ) {
                ListOwnership::Unique
            } else {
                ListOwnership::Shared
            };
        }
    }

    let Some(terminator) = block.terminator() else {
        return ListOwnershipTransfer {
            edges: Vec::new(),
            issues,
        };
    };
    match terminator.kind() {
        TerminatorKind::Invoke { arguments, .. } => {
            for argument in arguments.iter().copied() {
                if list_values.get(argument.index()).copied().unwrap_or(false) {
                    states[argument.index()] = ListOwnership::Shared;
                }
            }
        }
        TerminatorKind::Return(value)
            if list_values.get(value.index()).copied().unwrap_or(false) =>
        {
            states[value.index()] = ListOwnership::Shared;
        }
        _ => {}
    }
    for writeback in terminator.writebacks().iter().copied() {
        if list_values.get(writeback.index()).copied().unwrap_or(false) {
            states[writeback.index()] = ListOwnership::Shared;
        }
    }

    let mut edges = Vec::new();
    for (target, arguments) in forwarded_list_edges(terminator.kind()) {
        let Some(target_block) = function.block(target) else {
            continue;
        };
        let mut edge_states = states.clone();
        let implicit = target_block.params().len().saturating_sub(arguments.len());
        for parameter in target_block.params().iter().copied().take(implicit) {
            if list_values.get(parameter.index()).copied().unwrap_or(false) {
                edge_states[parameter.index()] = ListOwnership::Shared;
            }
        }
        let mut counts = BTreeMap::<ValueId, usize>::new();
        for argument in arguments.iter().copied() {
            if list_values.get(argument.index()).copied().unwrap_or(false) {
                *counts.entry(argument).or_default() += 1;
            }
        }
        for (argument, parameter) in arguments
            .iter()
            .copied()
            .zip(target_block.params().iter().copied().skip(implicit))
        {
            if !list_values.get(parameter.index()).copied().unwrap_or(false) {
                continue;
            }
            let incoming = states
                .get(argument.index())
                .copied()
                .unwrap_or(ListOwnership::Shared);
            edge_states[argument.index()] = ListOwnership::Consumed;
            edge_states[parameter.index()] = if counts.get(&argument).copied().unwrap_or(0) > 1 {
                ListOwnership::Shared
            } else {
                incoming
            };
        }
        edges.push(ListOwnershipEdge {
            target: target.index(),
            states: edge_states,
        });
    }
    ListOwnershipTransfer { edges, issues }
}

fn forwarded_list_edges(kind: &TerminatorKind) -> Vec<(BlockId, &[ValueId])> {
    match kind {
        TerminatorKind::Jump(target) => vec![(target.block, &target.arguments)],
        TerminatorKind::Branch {
            then_target,
            else_target,
            ..
        } => vec![
            (then_target.block, &then_target.arguments),
            (else_target.block, &else_target.arguments),
        ],
        TerminatorKind::SumSwitch { cases, .. } | TerminatorKind::DynSwitch { cases, .. } => cases
            .iter()
            .map(|case| (case.block, case.arguments.as_ref()))
            .collect(),
        TerminatorKind::CheckedIntNegate { normal, fault, .. }
        | TerminatorKind::CheckedIntBinary { normal, fault, .. }
        | TerminatorKind::TaskSleep { normal, fault, .. }
        | TerminatorKind::LogWrite { normal, fault, .. }
        | TerminatorKind::StdoutWrite { normal, fault, .. } => vec![
            (normal.block, &normal.arguments),
            (fault.block, &fault.arguments),
        ],
        TerminatorKind::Invoke { normal, unwind, .. } => vec![
            (normal.block, &normal.arguments),
            (unwind.block, &unwind.arguments),
        ],
        TerminatorKind::AwaitTasks {
            normal,
            fault,
            cancel,
            ..
        } => vec![
            (normal.block, normal.arguments.as_ref()),
            (fault.block, fault.arguments.as_ref()),
            (cancel.block, cancel.arguments.as_ref()),
        ],
        TerminatorKind::Assert { success, fault, .. } => vec![
            (success.block, &success.arguments),
            (fault.block, &fault.arguments),
        ],
        TerminatorKind::Return(_)
        | TerminatorKind::Fault { .. }
        | TerminatorKind::ResumeFault
        | TerminatorKind::TaskCancelled => Vec::new(),
    }
}

fn reachable_blocks(entry: usize, successors: &[Vec<usize>]) -> Vec<bool> {
    let mut reachable = vec![false; successors.len()];
    let mut pending = vec![entry];
    while let Some(block) = pending.pop() {
        let Some(is_reachable) = reachable.get_mut(block) else {
            continue;
        };
        if *is_reachable {
            continue;
        }
        *is_reachable = true;
        if let Some(next) = successors.get(block) {
            pending.extend(
                next.iter()
                    .copied()
                    .filter(|target| *target < successors.len()),
            );
        }
    }
    reachable
}

#[derive(Clone, Copy)]
enum ProofBranch {
    None,
    Unique { source: usize, true_target: usize },
    Ambiguous,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum UniquePredecessor {
    None,
    One(usize),
    Multiple,
}

struct IntegerProofFacts {
    branches: Vec<ProofBranch>,
    predecessors: Vec<UniquePredecessor>,
}

impl IntegerProofFacts {
    fn collect(function: &Function, reachable: &[bool], predecessors: &[Vec<usize>]) -> Self {
        let predecessors = predecessors
            .iter()
            .map(|incoming| {
                let mut incoming = incoming
                    .iter()
                    .copied()
                    .filter(|source| reachable.get(*source).copied().unwrap_or(false));
                let Some(first) = incoming.next() else {
                    return UniquePredecessor::None;
                };
                if incoming.next().is_some() {
                    UniquePredecessor::Multiple
                } else {
                    UniquePredecessor::One(first)
                }
            })
            .collect::<Vec<_>>();
        let mut branches = vec![ProofBranch::None; function.values.len()];
        for (source, block) in function.blocks.iter().enumerate() {
            if !reachable.get(source).copied().unwrap_or(false) {
                continue;
            }
            let Some(TerminatorKind::Branch {
                condition,
                then_target,
                ..
            }) = block.terminator.as_ref().map(Terminator::kind)
            else {
                continue;
            };
            if condition.owner() != function.id {
                continue;
            }
            let Some(slot) = branches.get_mut(condition.index()) else {
                continue;
            };
            *slot = match *slot {
                ProofBranch::None => ProofBranch::Unique {
                    source,
                    true_target: then_target.block.index(),
                },
                ProofBranch::Unique { .. } | ProofBranch::Ambiguous => ProofBranch::Ambiguous,
            };
        }
        Self {
            branches,
            predecessors,
        }
    }
}

fn is_exact_successor_proof(
    function: &Function,
    value: ValueId,
    upper_bound: ValueId,
    proof: ValueId,
) -> bool {
    function.value(proof).is_some_and(|proof_value| {
        let ValueDefinition::InstructionResult {
            instruction: producer,
            index: 0,
        } = proof_value.definition
        else {
            return false;
        };
        function.instruction(producer).is_some_and(|producer| {
            matches!(
                &producer.kind,
                InstructionKind::IntCompare {
                    predicate: crate::IntPredicate::Less,
                    left,
                    right,
                } if *left == value && *right == upper_bound
            )
        })
    })
}

#[derive(Clone, Copy)]
struct DominatorInterval {
    start: usize,
    end: usize,
}

struct DominatorTree {
    intervals: Vec<Option<DominatorInterval>>,
}

impl DominatorTree {
    fn dominates(&self, dominator: usize, block: usize) -> bool {
        let Some(Some(dominator)) = self.intervals.get(dominator) else {
            return false;
        };
        let Some(Some(block)) = self.intervals.get(block) else {
            return false;
        };
        dominator.start <= block.start && block.start < dominator.end
    }
}

/// Computes immediate dominators in reverse postorder, then assigns intervals
/// in the dominator tree. This avoids materializing one set per block and
/// makes every later dominance query constant time.
fn compute_dominators(
    entry: usize,
    reachable: &[bool],
    successors: &[Vec<usize>],
    predecessors: &[Vec<usize>],
) -> DominatorTree {
    let block_count = reachable.len();
    let order = reverse_postorder(entry, reachable, successors);
    let mut order_index = vec![usize::MAX; block_count];
    for (index, block) in order.iter().copied().enumerate() {
        if let Some(slot) = order_index.get_mut(block) {
            *slot = index;
        }
    }

    let mut immediate = vec![None; block_count];
    if entry < block_count {
        immediate[entry] = Some(entry);
    }
    loop {
        let mut changed = false;
        for block in order.iter().copied().skip(1) {
            let Some(incoming) = predecessors.get(block) else {
                continue;
            };
            let mut defined = incoming.iter().copied().filter(|predecessor| {
                reachable.get(*predecessor).copied().unwrap_or(false)
                    && immediate.get(*predecessor).is_some_and(Option::is_some)
            });
            let Some(first) = defined.next() else {
                continue;
            };
            let mut next = first;
            for predecessor in defined {
                let Some(common) =
                    intersect_dominators(next, predecessor, &immediate, &order_index)
                else {
                    continue;
                };
                next = common;
            }
            if immediate.get(block).copied().flatten() != Some(next)
                && let Some(slot) = immediate.get_mut(block)
            {
                *slot = Some(next);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut children = vec![Vec::new(); block_count];
    for (block, parent) in immediate.iter().copied().enumerate() {
        if block == entry || !reachable.get(block).copied().unwrap_or(false) {
            continue;
        }
        if let Some(parent) = parent
            && let Some(parent_children) = children.get_mut(parent)
        {
            parent_children.push(block);
        }
    }
    DominatorTree {
        intervals: dominator_intervals(entry, &children),
    }
}

fn reverse_postorder(entry: usize, reachable: &[bool], successors: &[Vec<usize>]) -> Vec<usize> {
    let mut visited = vec![false; successors.len()];
    let mut postorder = Vec::new();
    if !reachable.get(entry).copied().unwrap_or(false) {
        return postorder;
    }
    visited[entry] = true;
    let mut stack = vec![(entry, 0_usize)];
    while let Some((block, next_index)) = stack.last_mut() {
        let Some(next) = successors
            .get(*block)
            .and_then(|targets| targets.get(*next_index))
            .copied()
        else {
            postorder.push(*block);
            stack.pop();
            continue;
        };
        *next_index += 1;
        if visited.get(next).is_some_and(|seen| !seen) {
            visited[next] = true;
            stack.push((next, 0));
        }
    }
    postorder.reverse();
    postorder
}

fn intersect_dominators(
    mut left: usize,
    mut right: usize,
    immediate: &[Option<usize>],
    order_index: &[usize],
) -> Option<usize> {
    while left != right {
        while *order_index.get(left)? > *order_index.get(right)? {
            left = immediate.get(left).copied().flatten()?;
        }
        while *order_index.get(right)? > *order_index.get(left)? {
            right = immediate.get(right).copied().flatten()?;
        }
    }
    Some(left)
}

fn dominator_intervals(entry: usize, children: &[Vec<usize>]) -> Vec<Option<DominatorInterval>> {
    let mut intervals = vec![None; children.len()];
    if entry >= children.len() {
        return intervals;
    }
    let mut next = 0_usize;
    let mut stack = vec![(entry, false)];
    while let Some((block, exiting)) = stack.pop() {
        if exiting {
            if let Some(Some(interval)) = intervals.get_mut(block) {
                interval.end = next;
            }
            continue;
        }
        if let Some(slot) = intervals.get_mut(block) {
            *slot = Some(DominatorInterval {
                start: next,
                end: next,
            });
        }
        next = next.saturating_add(1);
        stack.push((block, true));
        if let Some(block_children) = children.get(block) {
            stack.extend(
                block_children
                    .iter()
                    .rev()
                    .copied()
                    .map(|child| (child, false)),
            );
        }
    }
    intervals
}

#[cfg(test)]
mod tests {
    use loom_mir::{ExprId as MirExprId, FunctionId as MirFunctionId, TypeId, WitnessId};

    use super::*;
    use crate::{
        Constant, INSTANCE_KEY_STRUCTURE_BUDGET, InstanceKey, InstanceWitnessArgument, Origin,
        ProgramBuilder, Signature, TargetLayout, Terminator,
    };

    fn declared_program(sources: &[u32]) -> Program {
        let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let unit_ty = builder.type_id(&Type::Unit).expect("Unit type");
        for source in sources {
            builder
                .declare_function(
                    Origin::synthetic(MirFunctionId(*source)),
                    format!("instance.{source}"),
                    Signature::new(Vec::new(), unit_ty),
                    Effects::NONE,
                )
                .expect("declare");
        }
        builder.finish()
    }

    fn structural_equality_program() -> Program {
        let source = MirFunctionId(89);
        let origin = Origin::synthetic(source);
        let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let integer = builder.type_id(&Type::Int).expect("Int");
        let boolean = builder.type_id(&Type::Bool).expect("Bool");
        let helper = builder
            .declare_instance(
                InstanceKey::structural_equality(source, Type::Int),
                origin,
                "structural.eq.int",
                Signature::new([integer, integer], boolean),
                Effects::NONE,
            )
            .expect("declare structural equality helper");
        {
            let mut function = builder.function(helper).expect("function builder");
            let entry = function.create_block().expect("entry");
            function.set_entry(entry).expect("set entry");
            function
                .append_block_parameter(entry, integer)
                .expect("left operand");
            function
                .append_block_parameter(entry, integer)
                .expect("right operand");
            let result = function
                .append_instruction(
                    entry,
                    InstructionKind::Constant(Constant::Bool(true)),
                    &[boolean],
                    origin,
                )
                .expect("result")[0];
            function
                .terminate(
                    entry,
                    Terminator::new(TerminatorKind::Return(result), origin),
                )
                .expect("return");
        }
        builder.finish()
    }

    fn process_environment_program(
        catalog_option: TypeId,
        result_identity: TypeId,
        variants: &[Box<[Type]>],
        missing_variant: u32,
        found_variant: u32,
    ) -> Program {
        let origin = Origin::synthetic(MirFunctionId(96));
        let mut builder = ProgramBuilder::with_canonical_types(
            TargetLayout::new(64).expect("target"),
            crate::CanonicalTypeCatalog {
                option: Some(catalog_option),
                ..crate::CanonicalTypeCatalog::default()
            },
        );
        let text = builder.add_managed_text_type().expect("managed Text type");
        let option_text = builder
            .add_sum_type(Type::Nominal(result_identity, vec![Type::Text]), variants)
            .expect("Option[Text] sum");
        let root = builder
            .declare_function(
                origin,
                "process.environment",
                Signature::new([text], option_text),
                Effects::MAY_COLLECT.with_implications(),
            )
            .expect("function");
        {
            let mut function = builder.function(root).expect("function builder");
            let entry = function.create_block().expect("entry");
            function.set_entry(entry).expect("set entry");
            let name = function
                .append_block_parameter(entry, text)
                .expect("name parameter");
            let result = function
                .append_instruction(
                    entry,
                    InstructionKind::ProcessEnvironment {
                        name,
                        missing_variant,
                        found_variant,
                    },
                    &[option_text],
                    origin,
                )
                .expect("environment lookup")[0];
            function
                .terminate(
                    entry,
                    Terminator::new(TerminatorKind::Return(result), origin),
                )
                .expect("return");
        }
        builder.finish()
    }

    #[test]
    fn process_environment_requires_canonical_option_text_and_exact_effects() {
        let option = TypeId(120);
        let valid = process_environment_program(
            option,
            option,
            &[Box::new([]), Box::new([Type::Text])],
            0,
            1,
        );
        validate_program(&valid).expect("canonical process environment program");

        let noncanonical = process_environment_program(
            option,
            TypeId(121),
            &[Box::new([]), Box::new([Type::Text])],
            0,
            1,
        );
        let errors =
            validate_program(&noncanonical).expect_err("noncanonical Option must be rejected");
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::TypeMismatch
                && error.message().contains("canonical Option[Text]")
        }));

        let mut missing_effect = valid;
        missing_effect.functions[0].effects = Effects::NONE;
        let errors = validate_program(&missing_effect)
            .expect_err("collecting process environment lookup needs exact effects");
        assert!(
            errors
                .as_slice()
                .iter()
                .any(|error| error.code() == ValidationCode::EffectMismatch)
        );
    }

    #[test]
    fn process_environment_requires_distinct_canonical_variant_shapes() {
        let option = TypeId(122);
        let same_variant = process_environment_program(
            option,
            option,
            &[Box::new([]), Box::new([Type::Text])],
            0,
            0,
        );
        let errors =
            validate_program(&same_variant).expect_err("duplicate variants must be rejected");
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::InstructionShape
                && error.message().contains("distinct missing and found")
        }));

        let wrong_found = process_environment_program(
            option,
            option,
            &[Box::new([]), Box::new([Type::Int])],
            0,
            1,
        );
        let errors =
            validate_program(&wrong_found).expect_err("wrong found payload must be rejected");
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::InstructionShape
                && error.message().contains("exactly one Text")
        }));
    }

    #[test]
    fn independent_validation_rechecks_structural_equality_role_contract() {
        let valid = structural_equality_program();
        validate_program(&valid).expect("valid structural equality helper");

        let boolean = valid
            .representations
            .type_id(&Type::Bool)
            .expect("Bool type");
        let mut wrong_signature = valid.clone();
        wrong_signature.functions[0].signature = Signature::new([], boolean);
        let errors = validate_program(&wrong_signature).expect_err("wrong signature must fail");
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::InstancePlan && error.path() == "function[0].signature"
        }));

        let mut wrong_effect = valid.clone();
        wrong_effect.functions[0].effects = Effects::MAY_COLLECT.with_implications();
        let errors = validate_program(&wrong_effect).expect_err("wrong effect must fail");
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::InstancePlan && error.path() == "function[0].effects"
        }));

        let mut coroutine = valid.clone();
        coroutine.functions[0].coroutine = Some(crate::CoroutinePlan::new(boolean, []));
        let errors = validate_program(&coroutine).expect_err("coroutine helper must fail");
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::InstancePlan && error.path() == "function[0].coroutine"
        }));

        let mut sourced = valid.clone();
        sourced.functions[0].origin.expression = Some(MirExprId(1));
        let errors = validate_program(&sourced).expect_err("source expression must fail");
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::InstancePlan && error.path() == "function[0].origin"
        }));

        let mut spanned = valid;
        spanned.functions[0].origin.span.range.end = 1;
        let errors = validate_program(&spanned).expect_err("source span must fail");
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::InstancePlan && error.path() == "function[0].origin"
        }));
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one manual graph keeps the valid loop phi and forged entry-alias certificate adjacent"
    )]
    fn unique_list_certificate_accepts_loop_phi_and_rejects_entry_alias() {
        let origin = Origin::synthetic(MirFunctionId(90));
        let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let list = builder
            .add_managed_list_type(Type::List(Box::new(Type::Int)))
            .expect("List[Int]");
        let integer = builder.type_id(&Type::Int).expect("Int");
        let boolean = builder.type_id(&Type::Bool).expect("Bool");
        let root = builder
            .declare_function(
                origin,
                "list.unique.loop",
                Signature::new([], list),
                Effects::MAY_COLLECT.with_implications(),
            )
            .expect("function");
        {
            let mut function = builder.function(root).expect("function builder");
            let entry = function.create_block().expect("entry");
            let header = function.create_block().expect("header");
            let body = function.create_block().expect("body");
            let exit = function.create_block().expect("exit");
            function.set_entry(entry).expect("set entry");
            let empty = function
                .append_instruction(
                    entry,
                    InstructionKind::ListConstruct {
                        elements: Box::new([]),
                    },
                    &[list],
                    origin,
                )
                .expect("empty")[0];
            function
                .terminate(
                    entry,
                    Terminator::new(
                        TerminatorKind::Jump(crate::BlockTarget::new(header, [empty])),
                        origin,
                    ),
                )
                .expect("preheader");
            let carried = function
                .append_block_parameter(header, list)
                .expect("carried List");
            let condition = function
                .append_instruction(
                    header,
                    InstructionKind::Constant(Constant::Bool(true)),
                    &[boolean],
                    origin,
                )
                .expect("condition")[0];
            function
                .terminate(
                    header,
                    Terminator::new(
                        TerminatorKind::Branch {
                            condition,
                            then_target: crate::BlockTarget::new(body, []),
                            else_target: crate::BlockTarget::new(exit, []),
                        },
                        origin,
                    ),
                )
                .expect("branch");
            let element = function
                .append_instruction(
                    body,
                    InstructionKind::Constant(Constant::Int(1)),
                    &[integer],
                    origin,
                )
                .expect("element")[0];
            let appended = function
                .append_trusted_instruction(
                    body,
                    InstructionKind::ListAppendUnique {
                        list: carried,
                        value: element,
                    },
                    &[list],
                    origin,
                )
                .expect("trusted append")[0];
            function
                .terminate(
                    body,
                    Terminator::new(
                        TerminatorKind::Jump(crate::BlockTarget::new(header, [appended])),
                        origin,
                    ),
                )
                .expect("backedge");
            function
                .terminate(
                    exit,
                    Terminator::new(TerminatorKind::Return(carried), origin),
                )
                .expect("return");
        }
        builder.finish_checked().expect("unique loop certificate");

        let origin = Origin::synthetic(MirFunctionId(91));
        let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let list = builder
            .add_managed_list_type(Type::List(Box::new(Type::Int)))
            .expect("List[Int]");
        let integer = builder.type_id(&Type::Int).expect("Int");
        let root = builder
            .declare_function(
                origin,
                "list.unique.forged",
                Signature::new([list], list),
                Effects::MAY_COLLECT.with_implications(),
            )
            .expect("function");
        {
            let mut function = builder.function(root).expect("function builder");
            let entry = function.create_block().expect("entry");
            function.set_entry(entry).expect("set entry");
            let shared = function
                .append_block_parameter(entry, list)
                .expect("shared parameter");
            let element = function
                .append_instruction(
                    entry,
                    InstructionKind::Constant(Constant::Int(1)),
                    &[integer],
                    origin,
                )
                .expect("element")[0];
            let appended = function
                .append_trusted_instruction(
                    entry,
                    InstructionKind::ListAppendUnique {
                        list: shared,
                        value: element,
                    },
                    &[list],
                    origin,
                )
                .expect("unchecked trusted append")[0];
            function
                .terminate(
                    entry,
                    Terminator::new(TerminatorKind::Return(appended), origin),
                )
                .expect("return");
        }
        let errors = validate_program(&builder.finish()).expect_err("shared receiver must fail");
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::InvalidListUniqueness
                && error.message().contains("not uniquely owned")
        }));
    }

    #[test]
    fn independent_effect_propagation_closes_long_chains_and_recursive_sccs() {
        const FUNCTION_COUNT: usize = 4_096;
        let all = Effects::MAY_FAULT
            .union(Effects::MAY_COLLECT)
            .union(Effects::MAY_SUSPEND)
            .with_implications();
        let mut local = vec![Effects::NONE; FUNCTION_COUNT];
        local[0] = all;
        let mut reverse = vec![Vec::new(); FUNCTION_COUNT];
        for caller in 1..FUNCTION_COUNT {
            reverse[caller - 1].push(EffectCaller {
                caller,
                propagates_fault: true,
            });
        }
        let closed = propagate_effects(local, &reverse);
        assert!(closed.iter().all(|effects| *effects == all));

        let local = vec![
            Effects::MAY_COLLECT,
            Effects::MAY_FAULT,
            Effects::MAY_SUSPEND,
            Effects::NONE,
        ];
        let reverse = vec![
            vec![EffectCaller {
                caller: 2,
                propagates_fault: true,
            }],
            vec![EffectCaller {
                caller: 0,
                propagates_fault: true,
            }],
            vec![EffectCaller {
                caller: 1,
                propagates_fault: true,
            }],
            Vec::new(),
        ];
        let closed = propagate_effects(local, &reverse);
        assert!(closed[..3].iter().all(|effects| *effects == all));
        assert_eq!(closed[3], Effects::NONE);
    }

    #[test]
    fn active_cleanup_call_edges_strip_only_the_fault_capability() {
        let callee = Effects::MAY_FAULT
            .union(Effects::MAY_COLLECT)
            .union(Effects::MAY_SUSPEND)
            .with_implications();
        let closed = propagate_effects(
            vec![Effects::NONE, callee],
            &[
                Vec::new(),
                vec![EffectCaller {
                    caller: 0,
                    propagates_fault: false,
                }],
            ],
        );
        assert_eq!(closed[0], callee.without(Effects::MAY_FAULT));
        assert_eq!(closed[1], callee);
    }

    #[test]
    fn trusted_refinement_still_requires_the_exact_declared_base_type() {
        let money = Type::Nominal(TypeId(112), Vec::new());
        let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let integer = builder.type_id(&Type::Int).expect("Int type");
        let money_id = builder
            .add_transparent_type(money, &Type::Float)
            .expect("transparent Money type");
        let function = builder
            .declare_function(
                Origin::synthetic(MirFunctionId(93)),
                "wrong_base",
                Signature::new(Vec::new(), money_id),
                Effects::NONE,
            )
            .expect("declare function");
        {
            let mut function_builder = builder.function(function).expect("function builder");
            let entry = function_builder.create_block().expect("entry");
            function_builder.set_entry(entry).expect("set entry");
            let raw = function_builder
                .append_instruction(
                    entry,
                    InstructionKind::Constant(Constant::Int(10)),
                    &[integer],
                    Origin::synthetic(MirFunctionId(93)),
                )
                .expect("wrong raw value")[0];
            let forged = function_builder
                .append_trusted_instruction(
                    entry,
                    InstructionKind::RefineProven { value: raw },
                    &[money_id],
                    Origin::synthetic(MirFunctionId(93)),
                )
                .expect("trusted validation fixture")[0];
            function_builder
                .terminate(
                    entry,
                    Terminator::new(
                        TerminatorKind::Return(forged),
                        Origin::synthetic(MirFunctionId(93)),
                    ),
                )
                .expect("return");
        }
        let errors = validate_program(&builder.finish()).expect_err("wrong base must be rejected");
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::TypeMismatch
                && error.message().contains("exact declared base")
        }));
    }

    #[test]
    fn trusted_invariant_opcode_cannot_target_an_ordinary_product() {
        let semantic = Type::Nominal(TypeId(113), Vec::new());
        let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let integer = builder.type_id(&Type::Int).expect("Int type");
        let ordinary = builder
            .add_pod_record_type(semantic, &[Type::Int])
            .expect("ordinary product");
        let function = builder
            .declare_function(
                Origin::synthetic(MirFunctionId(94)),
                "wrong_invariant_target",
                Signature::new(Vec::new(), ordinary),
                Effects::NONE,
            )
            .expect("declare function");
        {
            let mut function_builder = builder.function(function).expect("function builder");
            let entry = function_builder.create_block().expect("entry");
            function_builder.set_entry(entry).expect("set entry");
            let field = function_builder
                .append_instruction(
                    entry,
                    InstructionKind::Constant(Constant::Int(10)),
                    &[integer],
                    Origin::synthetic(MirFunctionId(94)),
                )
                .expect("field")[0];
            let forged = function_builder
                .append_trusted_instruction(
                    entry,
                    InstructionKind::InvariantRecordProven {
                        fields: Box::from([field]),
                    },
                    &[ordinary],
                    Origin::synthetic(MirFunctionId(94)),
                )
                .expect("trusted validation fixture")[0];
            function_builder
                .terminate(
                    entry,
                    Terminator::new(
                        TerminatorKind::Return(forged),
                        Origin::synthetic(MirFunctionId(94)),
                    ),
                )
                .expect("return");
        }
        let errors =
            validate_program(&builder.finish()).expect_err("wrong construction must be rejected");
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::TypeMismatch
                && error.message().contains("construction boundary")
        }));
    }

    #[test]
    fn oversized_trusted_product_constructions_stop_at_the_validation_budget() {
        let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let fields = vec![Type::Int; crate::repr::DIRECT_PRODUCT_MAX_STRUCTURAL_NODES + 1];
        let protected = builder
            .add_invariant_record_type(Type::Nominal(TypeId(114), Vec::new()), &fields)
            .expect("unchecked wide protected product");
        let function = builder
            .declare_function(
                Origin::synthetic(MirFunctionId(95)),
                "wide_invariant",
                Signature::new(Vec::new(), protected),
                Effects::NONE,
            )
            .expect("declare function");
        {
            let mut function_builder = builder.function(function).expect("function builder");
            let entry = function_builder.create_block().expect("entry");
            function_builder.set_entry(entry).expect("set entry");
            let mut result = None;
            for _ in 0..64 {
                result = Some(
                    function_builder
                        .append_trusted_instruction(
                            entry,
                            InstructionKind::InvariantRecordProven {
                                fields: Box::new([]),
                            },
                            &[protected],
                            Origin::synthetic(MirFunctionId(95)),
                        )
                        .expect("trusted validation fixture")[0],
                );
            }
            function_builder
                .terminate(
                    entry,
                    Terminator::new(
                        TerminatorKind::Return(result.expect("one result")),
                        Origin::synthetic(MirFunctionId(95)),
                    ),
                )
                .expect("return");
        }
        let errors =
            validate_program(&builder.finish()).expect_err("wide product must be rejected");
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::InstructionShape
                && error.message().contains("validation budget")
        }));
    }

    #[test]
    fn instance_plan_validates_dense_order_uniqueness_and_source_consistency() {
        let mut non_dense = declared_program(&[80]);
        non_dense.instances.entries[0].id =
            InstanceId::from_index(non_dense.brand, 1).expect("malformed id");
        let errors = validate_program(&non_dense).expect_err("non-dense plan must fail");
        assert!(
            errors
                .as_slice()
                .iter()
                .any(|error| error.code() == ValidationCode::IndexMismatch)
        );

        let mut duplicate = declared_program(&[81, 82]);
        duplicate.instances.entries[1].key = duplicate.instances.entries[0].key.clone();
        let errors = validate_program(&duplicate).expect_err("duplicate plan key must fail");
        assert!(
            errors
                .as_slice()
                .iter()
                .any(|error| error.code() == ValidationCode::InstancePlan)
        );

        let mut mismatched = declared_program(&[83]);
        mismatched.instances.entries[0].key = InstanceKey::monomorphic(MirFunctionId(84));
        let errors = validate_program(&mismatched).expect_err("source mismatch must fail");
        assert!(
            errors
                .as_slice()
                .iter()
                .any(|error| error.code() == ValidationCode::OriginMismatch)
        );
    }

    #[test]
    fn instance_plan_validation_enforces_the_non_recursive_structure_budget() {
        let mut program = declared_program(&[85]);
        let mut witness = InstanceWitnessArgument::Concrete(WitnessId(0));
        for _ in 0..INSTANCE_KEY_STRUCTURE_BUDGET {
            witness = InstanceWitnessArgument::apply(WitnessId(0), vec![witness]);
        }
        program.instances.entries[0].key =
            InstanceKey::new(MirFunctionId(85), Vec::new(), vec![witness]);

        let errors = validate_program(&program).expect_err("oversized key must fail");
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::InstanceKeyStructureBudget
                && error.path() == "instances[0].key"
        }));
    }

    #[test]
    fn instance_plan_validation_rejects_unresolved_compile_time_arguments() {
        let mut program = declared_program(&[86]);
        program.instances.entries[0].key = InstanceKey::new(
            MirFunctionId(86),
            vec![Type::AssociatedProjection {
                witness: 0,
                associated: "Item".into(),
            }],
            vec![InstanceWitnessArgument::Parameter(0)],
        );

        let errors = validate_program(&program).expect_err("open instance key must fail");
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::OpenInstanceKey && error.path() == "instances[0].key"
        }));
    }

    #[test]
    fn malformed_block_identity_does_not_enter_cfg_indices() {
        let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let unit_ty = builder.type_id(&Type::Unit).expect("Unit type");
        let function = builder
            .declare_function(
                Origin::synthetic(MirFunctionId(90)),
                "malformed.block_identity",
                Signature::new(Vec::new(), unit_ty),
                Effects::NONE,
            )
            .expect("declare");
        {
            let mut function_builder = builder.function(function).expect("builder");
            let entry = function_builder.create_block().expect("entry");
            let exit = function_builder.create_block().expect("exit");
            function_builder.set_entry(entry).expect("set entry");
            let unit = function_builder
                .append_instruction(
                    entry,
                    InstructionKind::Constant(Constant::Unit),
                    &[unit_ty],
                    Origin::synthetic(MirFunctionId(90)),
                )
                .expect("unit")[0];
            function_builder
                .terminate(
                    entry,
                    Terminator::new(
                        TerminatorKind::Jump(crate::BlockTarget::new(exit, Vec::new())),
                        Origin::synthetic(MirFunctionId(90)),
                    ),
                )
                .expect("jump");
            function_builder
                .terminate(
                    exit,
                    Terminator::new(
                        TerminatorKind::Return(unit),
                        Origin::synthetic(MirFunctionId(90)),
                    ),
                )
                .expect("return");
        }
        let mut program = builder.finish();
        program.functions[0].blocks[0].id =
            BlockId::from_index(function, 99).expect("malformed identity");

        let errors = validate_program(&program).expect_err("corruption must be rejected");
        assert!(
            errors
                .as_slice()
                .iter()
                .any(|error| error.code() == ValidationCode::IndexMismatch)
        );
    }

    #[test]
    fn reverse_table_order_linear_cfg_uses_bounded_dominator_state() {
        const BLOCK_COUNT: usize = 2_048;

        let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let unit_ty = builder.type_id(&Type::Unit).expect("Unit type");
        let function = builder
            .declare_function(
                Origin::synthetic(MirFunctionId(92)),
                "dominator.reverse_linear",
                Signature::new(Vec::new(), unit_ty),
                Effects::NONE,
            )
            .expect("declare");
        {
            let mut function_builder = builder.function(function).expect("builder");
            let blocks = (0..BLOCK_COUNT)
                .map(|_| function_builder.create_block().expect("block"))
                .collect::<Vec<_>>();
            function_builder.set_entry(blocks[0]).expect("set entry");
            let unit = function_builder
                .append_instruction(
                    blocks[0],
                    InstructionKind::Constant(Constant::Unit),
                    &[unit_ty],
                    Origin::synthetic(MirFunctionId(92)),
                )
                .expect("unit")[0];
            function_builder
                .terminate(
                    blocks[0],
                    Terminator::new(
                        TerminatorKind::Jump(crate::BlockTarget::new(
                            blocks[BLOCK_COUNT - 1],
                            Vec::new(),
                        )),
                        Origin::synthetic(MirFunctionId(92)),
                    ),
                )
                .expect("entry jump");
            for source in (2..BLOCK_COUNT).rev() {
                function_builder
                    .terminate(
                        blocks[source],
                        Terminator::new(
                            TerminatorKind::Jump(crate::BlockTarget::new(
                                blocks[source - 1],
                                Vec::new(),
                            )),
                            Origin::synthetic(MirFunctionId(92)),
                        ),
                    )
                    .expect("linear jump");
            }
            function_builder
                .terminate(
                    blocks[1],
                    Terminator::new(
                        TerminatorKind::Return(unit),
                        Origin::synthetic(MirFunctionId(92)),
                    ),
                )
                .expect("return");
        }

        builder
            .finish_checked()
            .expect("reverse table-order linear CFG is valid");
    }
}
