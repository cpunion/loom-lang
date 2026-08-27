use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::fmt;

use loom_mir::Type;

use crate::{
    BlockId, Constant, Effects, Function, InstanceId, Instruction, InstructionId, InstructionKind,
    ProductReprId, Program, Repr, RepresentationPlan, ResultTarget, SumReprId, SumTagRepr,
    Terminator, TerminatorKind, UnwindTarget, ValueDefinition, ValueId, ValueTypeId, ValueTypeKind,
};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ValidationCode {
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
}

impl ValidationCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
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
                | Repr::ManagedPointer => {}
            }
        }
        let mut product_value_uses = vec![0_usize; product_count];
        let mut sum_value_uses = vec![0_usize; sum_count];
        for (index, value_type) in representations.value_types().iter().enumerate() {
            match value_type.kind() {
                ValueTypeKind::Direct => {}
                ValueTypeKind::Transparent { base } => {
                    let valid = representations.value_type(base).is_some_and(|base_type| {
                        base_type.semantic() != &Type::Never
                            && representations
                                .repr(base_type.repr())
                                .is_some_and(|repr| !matches!(repr, Repr::Uninhabited))
                            && base.index() < index
                            && base_type.semantic() != value_type.semantic()
                            && base_type.repr() == value_type.repr()
                            && matches!(
                                value_type.semantic(),
                                Type::Nominal(_, arguments) if arguments.is_empty()
                            )
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
                    if !matches!(
                        value_type.semantic(),
                        Type::Nominal(_, arguments) if arguments.is_empty()
                    ) || !matches!(
                        representations.repr(value_type.repr()),
                        Some(Repr::Product(_))
                    ) {
                        self.error(
                            ValidationCode::RepresentationPlan,
                            format!("representations.type[{index}].invariant_product"),
                            "invariant-product type must be a monomorphic nominal value backed by a product representation",
                        );
                    }
                }
            }
            let canonical_protection = representations
                .type_id(value_type.semantic())
                .and_then(|canonical| representations.value_type(canonical))
                .is_some_and(|canonical| match (canonical.kind(), value_type.kind()) {
                    (ValueTypeKind::Direct, ValueTypeKind::Direct)
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
            match representations.repr(value_type.repr()).copied() {
                Some(Repr::Product(product)) => {
                    match value_type.semantic() {
                        Type::Nominal(_, arguments) if arguments.is_empty() => {}
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
                                "direct product value types require a structural tuple or monomorphic nominal semantic type",
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
                Some(Repr::ImmortalText | Repr::ManagedPointer) => {
                    if value_type.semantic() != &Type::Text
                        || value_type.kind() != ValueTypeKind::Direct
                        || representations.target().pointer_bits() != 64
                    {
                        self.error(
                            ValidationCode::RepresentationPlan,
                            format!("representations.type[{index}].text_pointer"),
                            "Text pointers must be the direct Text semantic type on a 64-bit target",
                        );
                    }
                }
                Some(Repr::Uninhabited | Repr::Zst | Repr::Scalar(_)) | None => {
                    if value_type.semantic() == &Type::Text {
                        self.error(
                            ValidationCode::RepresentationPlan,
                            format!("representations.type[{index}]"),
                            "Text semantic values must use one canonical Text pointer representation",
                        );
                    }
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
                    | Repr::Product(_)
                    | Repr::Sum(_) => None,
                })
        };
        let supported_field = |field: ValueTypeId| {
            representations.value_type(field).is_some_and(|value_type| {
                value_type.semantic() != &Type::Never
                    && matches!(
                        representations.repr(value_type.repr()),
                        Some(Repr::Zst | Repr::Scalar(_) | Repr::Product(_) | Repr::Sum(_))
                    )
            })
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
                if !supported_field(field) {
                    self.error(
                        ValidationCode::RepresentationPlan,
                        format!("representations.product[{index}].field[{field_index}]"),
                        "POD product fields must reference inhabited direct value types",
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
                    if !supported_field(field) {
                        self.error(
                            ValidationCode::RepresentationPlan,
                            format!(
                                "representations.sum[{index}].variant[{variant_index}].field[{field_index}]"
                            ),
                            "sum payloads must reference inhabited direct value types",
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
                    ValueTypeKind::Direct | ValueTypeKind::InvariantProduct => {
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
                            | Repr::ManagedPointer,
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
                        | Repr::ManagedPointer,
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
        self.validate_signature(function, &base);
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
            if let Some(terminator) = function
                .blocks
                .get(index)
                .and_then(|block| block.terminator.as_ref())
            {
                self.validate_terminator_fault_state(terminator, state, index, &base);
            }
        }
        let dominators = compute_dominators(entry.index(), &reachable, &successors, &predecessors);
        self.validate_dominance(function, &base, &schedule, &reachable, &dominators);
        self.validate_integer_proofs(function, &base, &reachable, &predecessors, &dominators);
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
            if self.product_fields(ty).is_none() {
                self.error(
                    ValidationCode::InOutShape,
                    format!("{base}.signature.inout[{writeback_index}]"),
                    format!("inout parameter {parameter} must use a direct product value type"),
                );
            }
        }
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
        let text = self.scalar_type(&Type::Text);
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
            }
            InstructionKind::ProductInsert {
                aggregate,
                field,
                value,
            } => {
                let aggregate_type = function.value(*aggregate).map(|value| value.ty);
                if aggregate_type
                    .and_then(|ty| self.program.representations.value_type(ty))
                    .is_some_and(|ty| ty.kind() != ValueTypeKind::Direct)
                {
                    self.error(
                        ValidationCode::TypeMismatch,
                        format!("{path}.aggregate"),
                        "product insertion cannot mutate a transparent or invariant-protected semantic value",
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
                        ValueTypeKind::Direct | ValueTypeKind::InvariantProduct => None,
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
                        ValueTypeKind::Direct | ValueTypeKind::InvariantProduct => None,
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
            TerminatorKind::Return(_) | TerminatorKind::Fault { .. } | TerminatorKind::ResumeFault
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
            TerminatorKind::Return(value) => {
                self.require_value_type(
                    function,
                    *value,
                    function.signature.result(),
                    ValidationCode::ReturnType,
                    format!("{path}.value"),
                );
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
                self.validate_contract_fault_metadata(metadata, &format!("{path}.metadata"));
                self.require_may_fault_effect(function, &path, "assert");
            }
            TerminatorKind::Fault { metadata } => {
                if let crate::FaultMetadata::Contract(metadata) = metadata {
                    self.validate_contract_fault_metadata(metadata, &format!("{path}.metadata"));
                }
                self.require_may_fault_effect(function, &path, "fault");
            }
            TerminatorKind::ResumeFault => {
                self.require_may_fault_effect(function, &path, "resume_fault");
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

    fn validate_contract_fault_metadata(
        &mut self,
        metadata: &crate::ContractFaultMetadata,
        path: &str,
    ) {
        let user_code_in_budget = self.validate_contract_fault_text(metadata, path);
        for (name, span) in [
            ("contract_span", metadata.contract_span()),
            ("blame_span", metadata.blame_span()),
        ] {
            self.validate_contract_fault_span(span, &format!("{path}.{name}"));
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
        if metadata.contract_span() != metadata.blame_span() {
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
            && metadata.contract_span() != metadata.blame_span()
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
            TerminatorKind::Return(_) | TerminatorKind::Fault { .. }
                if state == FaultStateSet::ACTIVE =>
            {
                self.error(
                    ValidationCode::FaultState,
                    path,
                    "an active source fault cannot return normally or originate a second terminal fault; propagate it with resume_fault",
                );
            }
            TerminatorKind::ResumeFault if state == FaultStateSet::INACTIVE => {
                self.error(
                    ValidationCode::FaultState,
                    path,
                    "resume_fault requires an active source fault from an unwind edge",
                );
            }
            _ => {}
        }
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

/// Computes the least transitive function effects from operation and call
/// edges. Operations in active cleanup can only preserve the primary fault or
/// suppress a secondary one, so those paths strip only `MAY_FAULT`; runtime,
/// collection, executor, and suspension capabilities still propagate.
fn compute_exact_effects(program: &Program, fault_states: &[Vec<FaultStateSet>]) -> Vec<Effects> {
    let function_count = program.functions.len();
    let mut reverse_calls = vec![Vec::new(); function_count];
    let mut effects = vec![Effects::NONE; function_count];

    for (caller, function) in program.functions.iter().enumerate() {
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
                if matches!(instruction.kind(), InstructionKind::TextConcat { .. }) {
                    effects[caller] = effects[caller].union(Effects::MAY_COLLECT);
                }
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
                | TerminatorKind::Assert { .. }
                | TerminatorKind::Fault { .. }
                    if propagates_fault =>
                {
                    effects[caller] = effects[caller].union(Effects::MAY_FAULT);
                }
                TerminatorKind::Invoke { callee, .. } => {
                    if let Some(callee) = canonical_function_index(program, *callee) {
                        reverse_calls[callee].push(EffectCaller {
                            caller,
                            propagates_fault,
                        });
                    }
                }
                TerminatorKind::CheckedIntNegate { .. }
                | TerminatorKind::CheckedIntBinary { .. }
                | TerminatorKind::Assert { .. }
                | TerminatorKind::Fault { .. }
                | TerminatorKind::Jump(_)
                | TerminatorKind::Branch { .. }
                | TerminatorKind::SumSwitch { .. }
                | TerminatorKind::Return(_)
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
    use loom_mir::{FunctionId as MirFunctionId, TypeId, WitnessId};

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
        let money = Type::Nominal(TypeId(12), Vec::new());
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
        let semantic = Type::Nominal(TypeId(13), Vec::new());
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
            .add_invariant_record_type(Type::Nominal(TypeId(14), Vec::new()), &fields)
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
