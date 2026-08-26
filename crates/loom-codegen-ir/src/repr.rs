use std::error::Error;
use std::fmt;

use loom_mir::Type;

use crate::ids::ProgramBrand;
use crate::{ReprId, ValueTypeId};

/// Target facts used by the scalar LCIR foundation.
///
/// Optimization policy and CPU tuning deliberately do not belong here. They
/// may change generated instructions without changing scalar representations.
/// Aggregate lowering must extend this boundary with alignment, byte order,
/// and address-space facts before it selects any aggregate representation.
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

/// A target-level register representation used by the scalar LCIR foundation.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ScalarRepr {
    I1,
    I64,
    F64,
}

/// The canonical physical representation of one concrete Loom value type.
///
/// This initial vocabulary is intentionally small. Aggregate, managed, list,
/// dynamic-witness, and task representations will be added only alongside
/// their complete lowering and validation rules.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Repr {
    /// Control-flow vocabulary for semantic `Never`. The scalar foundation
    /// does not permit this representation in a function signature or SSA
    /// value; noreturn operations must be modeled by dedicated terminators.
    Uninhabited,
    Zst,
    Scalar(ScalarRepr),
}

/// A semantic Loom type paired with one selected target representation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValueType {
    semantic: Type,
    repr: ReprId,
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
    types: Vec<ValueType>,
}

impl RepresentationPlan {
    /// Creates the deterministic scalar representation vocabulary.
    #[must_use]
    pub fn scalar(target: TargetLayout) -> Self {
        Self::scalar_with_brand(target, ProgramBrand::fresh())
    }

    pub(crate) fn scalar_with_brand(target: TargetLayout, brand: ProgramBrand) -> Self {
        let reprs = vec![
            Repr::Uninhabited,
            Repr::Zst,
            Repr::Scalar(ScalarRepr::I1),
            Repr::Scalar(ScalarRepr::I64),
            Repr::Scalar(ScalarRepr::F64),
        ];
        let types = [
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
                .expect("the fixed scalar representation table fits in u32"),
        })
        .collect();
        Self {
            brand,
            target,
            reprs,
            types,
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
    pub fn value_types(&self) -> &[ValueType] {
        &self.types
    }

    #[must_use]
    pub fn repr(&self, id: ReprId) -> Option<&Repr> {
        (id.brand() == self.brand)
            .then(|| self.reprs.get(id.index()))
            .flatten()
    }

    #[must_use]
    pub fn value_type(&self, id: ValueTypeId) -> Option<&ValueType> {
        (id.brand() == self.brand)
            .then(|| self.types.get(id.index()))
            .flatten()
    }

    /// Returns the canonical LCIR type for a supported semantic scalar.
    #[must_use]
    pub fn type_id(&self, semantic: &Type) -> Option<ValueTypeId> {
        self.types
            .iter()
            .position(|candidate| candidate.semantic() == semantic)
            .and_then(|index| ValueTypeId::from_index(self.brand, index))
    }
}

#[cfg(test)]
mod tests {
    use loom_mir::FunctionId as MirFunctionId;

    use super::*;
    use crate::{
        Constant, Effects, InstructionKind, Origin, ProgramBuilder, Signature, Terminator,
        TerminatorKind, ValidationCode, validate_program,
    };

    #[test]
    fn malformed_scalar_catalog_reports_errors_without_panicking() {
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
}
