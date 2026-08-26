use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use loom_mir::Type;

use crate::ids::ProgramBrand;
use crate::{ProductReprId, ReprId, ValueTypeId};

pub(crate) const DIRECT_PRODUCT_MAX_NESTING_DEPTH: usize = 256;
pub(crate) const DIRECT_PRODUCT_MAX_STRUCTURAL_NODES: usize = 256;

/// Target facts used by the direct LCIR foundation.
///
/// Optimization policy and CPU tuning deliberately do not belong here. They
/// may change generated instructions without changing representations. Direct
/// register products delegate their ABI layout to LLVM's target data. A later
/// representation with an explicit byte or address-space layout must extend
/// this boundary with the facts that participate in that choice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TargetLayout {
    pointer_bits: u16,
}

impl TargetLayout {
    /// Constructs a target layout from an LLVM target's pointer width.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero, non-byte-sized, or implausibly wide
    /// pointer. LCIR currently needs no other target-dependent fact.
    pub fn new(pointer_bits: u16) -> Result<Self, TargetLayoutError> {
        if pointer_bits == 0 || !pointer_bits.is_multiple_of(8) || pointer_bits > 128 {
            return Err(TargetLayoutError { pointer_bits });
        }
        Ok(Self { pointer_bits })
    }

    #[must_use]
    pub const fn pointer_bits(self) -> u16 {
        self.pointer_bits
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TargetLayoutError {
    pointer_bits: u16,
}

impl fmt::Display for TargetLayoutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "LCIR pointer width must be a nonzero whole number of bytes no larger than 128 bits, got {}",
            self.pointer_bits
        )
    }
}

impl Error for TargetLayoutError {}

/// A target-level register representation used by the direct LCIR foundation.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ScalarRepr {
    I1,
    I64,
    F64,
}

/// The canonical physical representation of one concrete Loom value type.
///
/// This initial vocabulary is intentionally small. Managed, list,
/// dynamic-witness, and task representations will be added only alongside
/// their complete lowering and validation rules.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Repr {
    /// Control-flow vocabulary for semantic `Never`. The direct foundation
    /// does not permit this representation in a function signature or SSA
    /// value; noreturn operations must be modeled by dedicated terminators.
    Uninhabited,
    Zst,
    Scalar(ScalarRepr),
    /// An immutable register aggregate whose ordered fields are independently
    /// typed LCIR values. Closed products may contain other products, but
    /// validation rejects missing, uninhabited, or cyclic field graphs.
    Product(ProductReprId),
}

/// Ordered fields of one compiler-private product representation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductRepr {
    fields: Box<[ValueTypeId]>,
}

impl ProductRepr {
    #[must_use]
    pub const fn fields(&self) -> &[ValueTypeId] {
        &self.fields
    }
}

/// A semantic Loom type paired with one selected target representation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValueType {
    semantic: Type,
    repr: ReprId,
}

/// One representation selected by the plan for ordinary SSA lowering of a
/// semantic type.
///
/// [`ValueType`] entries are representation alternatives and therefore need
/// not be unique by semantic type. This separate registration table is the
/// explicit lookup key used by the current lowering plan. Future storage or
/// ABI plans may select other alternatives without changing or duplicating
/// this canonical value registration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TypeRegistration {
    semantic: Type,
    value_type: ValueTypeId,
}

impl TypeRegistration {
    #[must_use]
    pub const fn semantic(&self) -> &Type {
        &self.semantic
    }

    #[must_use]
    pub const fn value_type(&self) -> ValueTypeId {
        self.value_type
    }
}

impl ValueType {
    #[must_use]
    pub const fn semantic(&self) -> &Type {
        &self.semantic
    }

    #[must_use]
    pub const fn repr(&self) -> ReprId {
        self.repr
    }
}

/// Representation vocabulary selected for one LCIR program.
///
/// The foundation has one canonical entry for each primitive scalar. This does
/// not require every future aggregate to have one global representation: an
/// explicit planner may add distinct value representations and conversions.
/// ABI passing modes and storage classes remain separate decisions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepresentationPlan {
    brand: ProgramBrand,
    target: TargetLayout,
    reprs: Vec<Repr>,
    products: Vec<ProductRepr>,
    types: Vec<ValueType>,
    registrations: Vec<TypeRegistration>,
    canonical_types: BTreeMap<Type, ValueTypeId>,
}

impl RepresentationPlan {
    /// Creates the deterministic baseline direct representation vocabulary.
    #[must_use]
    pub fn direct(target: TargetLayout) -> Self {
        Self::direct_with_brand(target, ProgramBrand::fresh())
    }

    pub(crate) fn direct_with_brand(target: TargetLayout, brand: ProgramBrand) -> Self {
        let reprs = vec![
            Repr::Uninhabited,
            Repr::Zst,
            Repr::Scalar(ScalarRepr::I1),
            Repr::Scalar(ScalarRepr::I64),
            Repr::Scalar(ScalarRepr::F64),
        ];
        let types: Vec<ValueType> = [
            (Type::Never, 0_u32),
            (Type::Unit, 1),
            (Type::Bool, 2),
            (Type::Int, 3),
            (Type::Float, 4),
        ]
        .into_iter()
        .map(|(semantic, repr)| ValueType {
            semantic,
            repr: ReprId::from_index(brand, repr as usize)
                .expect("the fixed primitive representation table fits in u32"),
        })
        .collect();
        let registrations: Vec<TypeRegistration> = types
            .iter()
            .enumerate()
            .map(|(index, value_type)| TypeRegistration {
                semantic: value_type.semantic.clone(),
                value_type: ValueTypeId::from_index(brand, index)
                    .expect("the fixed primitive value-type table fits in u32"),
            })
            .collect();
        let canonical_types = registrations
            .iter()
            .map(|registration| (registration.semantic.clone(), registration.value_type))
            .collect();
        Self {
            brand,
            target,
            reprs,
            products: Vec::new(),
            types,
            registrations,
            canonical_types,
        }
    }

    #[must_use]
    pub const fn target(&self) -> TargetLayout {
        self.target
    }

    #[must_use]
    pub fn reprs(&self) -> &[Repr] {
        &self.reprs
    }

    #[must_use]
    pub fn products(&self) -> &[ProductRepr] {
        &self.products
    }

    #[must_use]
    pub fn value_types(&self) -> &[ValueType] {
        &self.types
    }

    #[must_use]
    pub fn registrations(&self) -> &[TypeRegistration] {
        &self.registrations
    }

    pub(crate) const fn canonical_types(&self) -> &BTreeMap<Type, ValueTypeId> {
        &self.canonical_types
    }

    #[must_use]
    pub fn repr(&self, id: ReprId) -> Option<&Repr> {
        (id.brand() == self.brand)
            .then(|| self.reprs.get(id.index()))
            .flatten()
    }

    #[must_use]
    pub fn product(&self, id: ProductReprId) -> Option<&ProductRepr> {
        (id.brand() == self.brand)
            .then(|| self.products.get(id.index()))
            .flatten()
    }

    #[must_use]
    pub fn value_type(&self, id: ValueTypeId) -> Option<&ValueType> {
        (id.brand() == self.brand)
            .then(|| self.types.get(id.index()))
            .flatten()
    }

    /// Returns the canonical LCIR type for a registered semantic type.
    #[must_use]
    pub fn type_id(&self, semantic: &Type) -> Option<ValueTypeId> {
        self.canonical_types.get(semantic).copied()
    }

    fn add_product(&mut self, semantic: Type, fields: &[Type]) -> Option<ValueTypeId> {
        if self.type_id(&semantic).is_some() {
            return None;
        }
        let fields = fields
            .iter()
            .map(|field| self.type_id(field))
            .collect::<Option<Vec<_>>>()?;
        let product = ProductReprId::from_index(self.brand, self.products.len())?;
        let repr = ReprId::from_index(self.brand, self.reprs.len())?;
        let ty = ValueTypeId::from_index(self.brand, self.types.len())?;
        self.products.push(ProductRepr {
            fields: fields.into_boxed_slice(),
        });
        self.reprs.push(Repr::Product(product));
        self.types.push(ValueType {
            semantic: semantic.clone(),
            repr,
        });
        self.registrations.push(TypeRegistration {
            semantic: semantic.clone(),
            value_type: ty,
        });
        self.canonical_types.insert(semantic, ty);
        Some(ty)
    }

    pub(crate) fn add_pod_record(
        &mut self,
        semantic: Type,
        fields: &[Type],
    ) -> Option<ValueTypeId> {
        if !matches!(&semantic, Type::Nominal(_, arguments) if arguments.is_empty()) {
            return None;
        }
        self.add_product(semantic, fields)
    }

    pub(crate) fn add_tuple(&mut self, elements: &[Type]) -> Option<ValueTypeId> {
        self.add_product(Type::Tuple(elements.to_vec()), elements)
    }
}

#[cfg(test)]
mod tests {
    use loom_mir::{FunctionId as MirFunctionId, TypeId};

    use super::*;
    use crate::{
        Constant, Effects, InstructionKind, Origin, ProgramBuilder, Signature, Terminator,
        TerminatorKind, ValidationCode, validate_program,
    };

    #[test]
    fn malformed_direct_catalog_reports_errors_without_panicking() {
        let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let unit_ty = builder.type_id(&Type::Unit).expect("Unit type");
        let function = builder
            .declare_function(
                Origin::synthetic(MirFunctionId(91)),
                "malformed.representations",
                Signature::new(Vec::new(), unit_ty),
                Effects::NONE,
            )
            .expect("declare");
        {
            let mut function_builder = builder.function(function).expect("builder");
            let entry = function_builder.create_block().expect("entry");
            function_builder.set_entry(entry).expect("set entry");
            let unit = function_builder
                .append_instruction(
                    entry,
                    InstructionKind::Constant(Constant::Unit),
                    &[unit_ty],
                    Origin::synthetic(MirFunctionId(91)),
                )
                .expect("unit")[0];
            function_builder
                .terminate(
                    entry,
                    Terminator::new(
                        TerminatorKind::Return(unit),
                        Origin::synthetic(MirFunctionId(91)),
                    ),
                )
                .expect("return");
        }
        let mut program = builder.finish();
        program.representations.types.clear();

        let errors = validate_program(&program).expect_err("corruption must be rejected");
        assert!(
            errors
                .as_slice()
                .iter()
                .any(|error| error.code() == ValidationCode::RepresentationPlan)
        );
    }

    #[test]
    fn semantic_value_type_alternatives_do_not_duplicate_the_canonical_registration() {
        let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let unit_ty = builder.type_id(&Type::Unit).expect("Unit type");
        let function = builder
            .declare_function(
                Origin::synthetic(MirFunctionId(92)),
                "alternative.representation",
                Signature::new(Vec::new(), unit_ty),
                Effects::NONE,
            )
            .expect("declare");
        {
            let mut function_builder = builder.function(function).expect("builder");
            let entry = function_builder.create_block().expect("entry");
            function_builder.set_entry(entry).expect("set entry");
            let unit = function_builder
                .append_instruction(
                    entry,
                    InstructionKind::Constant(Constant::Unit),
                    &[unit_ty],
                    Origin::synthetic(MirFunctionId(92)),
                )
                .expect("unit")[0];
            function_builder
                .terminate(
                    entry,
                    Terminator::new(
                        TerminatorKind::Return(unit),
                        Origin::synthetic(MirFunctionId(92)),
                    ),
                )
                .expect("return");
        }
        let mut program = builder.finish();
        let int = program
            .representations
            .type_id(&Type::Int)
            .and_then(|ty| program.representations.value_type(ty))
            .cloned()
            .expect("Int representation");
        program.representations.types.push(int);

        validate_program(&program)
            .expect("an unregistered semantic alternative must not violate plan uniqueness");
        assert_eq!(
            program
                .representations
                .type_id(&Type::Int)
                .map(ValueTypeId::raw),
            Some(3),
            "canonical lookup remains fixed by its explicit registration"
        );
    }

    #[test]
    fn duplicate_canonical_registration_is_rejected_by_its_explicit_key() {
        let builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let mut program = builder.finish();
        let duplicate = program.representations.registrations[3].clone();
        program.representations.registrations.push(duplicate);

        let errors = validate_program(&program).expect_err("duplicate registration must fail");
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::RepresentationPlan
                && error.path().contains("registration")
        }));
    }

    #[test]
    fn unregistered_product_alternative_does_not_compete_for_the_canonical_key() {
        let semantic = Type::Nominal(TypeId(70), Vec::new());
        let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let canonical = builder
            .add_pod_record_type(semantic.clone(), &[Type::Int])
            .expect("canonical product");
        let mut program = builder.finish();
        let canonical_value = program
            .representations
            .value_type(canonical)
            .cloned()
            .expect("canonical value type");
        let Repr::Product(canonical_product) = program
            .representations
            .repr(canonical_value.repr)
            .copied()
            .expect("canonical representation")
        else {
            panic!("record must use a product representation")
        };
        let alternative_product =
            ProductReprId::from_index(program.brand, program.representations.products.len())
                .expect("alternative product identity");
        let alternative_repr =
            ReprId::from_index(program.brand, program.representations.reprs.len())
                .expect("alternative representation identity");
        let alternative_type =
            ValueTypeId::from_index(program.brand, program.representations.types.len())
                .expect("alternative value type identity");
        program
            .representations
            .products
            .push(program.representations.products[canonical_product.index()].clone());
        program
            .representations
            .reprs
            .push(Repr::Product(alternative_product));
        program.representations.types.push(ValueType {
            semantic: semantic.clone(),
            repr: alternative_repr,
        });

        validate_program(&program)
            .expect("an unregistered product alternative must be a valid representation choice");
        assert_eq!(
            program.representations.type_id(&semantic),
            Some(canonical),
            "the explicit registration must keep selecting the canonical representation"
        );
        assert_ne!(canonical, alternative_type);
    }

    #[test]
    fn cyclic_product_representation_graph_is_rejected() {
        let first_semantic = Type::Nominal(TypeId(71), Vec::new());
        let second_semantic = Type::Nominal(TypeId(72), Vec::new());
        let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let first = builder
            .add_pod_record_type(first_semantic.clone(), &[Type::Int])
            .expect("first product");
        let second = builder
            .add_pod_record_type(second_semantic, &[first_semantic])
            .expect("second product");
        let mut program = builder.finish();
        let Some(Repr::Product(first_product)) = program
            .representations
            .value_type(first)
            .and_then(|value_type| program.representations.repr(value_type.repr))
            .copied()
        else {
            panic!("first record must use a product representation")
        };
        program.representations.products[first_product.index()].fields = Box::from([second]);

        let errors = validate_program(&program).expect_err("product cycles must be rejected");
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::RepresentationPlan && error.message().contains("cycle")
        }));
    }

    #[test]
    fn direct_product_depth_and_closure_budgets_are_validated() {
        let mut deep = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let mut child = Type::Int;
        for index in 0..=DIRECT_PRODUCT_MAX_NESTING_DEPTH {
            let semantic = Type::Nominal(
                TypeId(u32::try_from(100 + index).expect("test type identity")),
                Vec::new(),
            );
            deep.add_pod_record_type(semantic.clone(), &[child])
                .expect("deep product");
            child = semantic;
        }
        let deep_errors =
            validate_program(&deep.finish()).expect_err("over-deep products must fail validation");
        assert!(deep_errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::RepresentationPlan
                && error.message().contains("structural budget")
        }));

        let mut wide = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let fields = vec![Type::Int; DIRECT_PRODUCT_MAX_STRUCTURAL_NODES + 1];
        wide.add_pod_record_type(Type::Nominal(TypeId(2_000), Vec::new()), &fields)
            .expect("wide product");
        let wide_errors =
            validate_program(&wide.finish()).expect_err("over-wide products must fail validation");
        assert!(wide_errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::RepresentationPlan
                && error.message().contains("structural budget")
        }));

        let mut wide_tuple = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        wide_tuple
            .add_tuple_type(&vec![Type::Int; DIRECT_PRODUCT_MAX_STRUCTURAL_NODES])
            .expect("unchecked builder admits a tuple for independent validation");
        let tuple_errors = validate_program(&wide_tuple.finish())
            .expect_err("an over-wide tuple must fail independent validation");
        assert!(tuple_errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::RepresentationPlan
                && error.message().contains("structural budget")
        }));
    }

    #[test]
    fn structural_tuples_share_product_operations_but_validate_their_semantics() {
        let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let inner_semantic = Type::Tuple(vec![Type::Int, Type::Bool]);
        let inner = builder
            .add_tuple_type(&[Type::Int, Type::Bool])
            .expect("inner tuple");
        let record_semantic = Type::Nominal(TypeId(2_100), Vec::new());
        builder
            .add_pod_record_type(
                record_semantic.clone(),
                std::slice::from_ref(&inner_semantic),
            )
            .expect("record containing a tuple");
        let outer = builder
            .add_tuple_type(&[record_semantic, Type::Float])
            .expect("tuple containing a record");
        assert_ne!(inner, outer);
        validate_program(&builder.finish()).expect("nested tuple/record products");

        let mut malformed = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let tuple = malformed
            .add_tuple_type(&[Type::Int, Type::Bool])
            .expect("tuple");
        let mut malformed = malformed.finish();
        malformed.representations.types[tuple.index()].semantic =
            Type::Tuple(vec![Type::Int, Type::Float]);
        let errors = validate_program(&malformed)
            .expect_err("tuple element semantics must match product field types");
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::RepresentationPlan
                && error.message().contains("semantic element")
        }));
    }

    #[test]
    fn canonical_type_index_matches_registrations_at_scale_and_rejects_staleness() {
        const RECORDS: usize = 4_096;

        let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let mut semantics = Vec::with_capacity(RECORDS);
        for index in 0..RECORDS {
            let semantic = Type::Nominal(
                TypeId(u32::try_from(10_000 + index).expect("test type identity")),
                Vec::new(),
            );
            let registered = builder
                .add_pod_record_type(semantic.clone(), &[Type::Int])
                .expect("independent product");
            assert_eq!(builder.type_id(&semantic), Some(registered));
            semantics.push(semantic);
        }
        let program = builder.finish();
        validate_program(&program).expect("large independent product plan");
        for semantic in &semantics {
            assert!(program.representations.type_id(semantic).is_some());
        }

        let mut stale = program;
        stale
            .representations
            .registrations
            .pop()
            .expect("record registration");
        let errors = validate_program(&stale).expect_err("a stale canonical index must fail");
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::RepresentationPlan
                && error.path().contains("canonical_types")
        }));
    }
}
