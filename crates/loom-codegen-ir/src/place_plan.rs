use std::error::Error;
use std::fmt;

use loom_mir::{LocalId, Place};

use crate::{Repr, ReprId, RepresentationPlan, ValueTypeId, ValueTypeKind};

/// Maximum number of record fields traversed by one lowered place.
///
/// This is deliberately lower than the aggregate representation budget. It
/// bounds both classifier work and the `extractvalue`/`insertvalue` chain
/// emitted for one source operation.
pub(crate) const PLACE_MAX_PROJECTION_DEPTH: usize = 64;

/// Maximum aggregate reconstruction work admitted for one complete artifact.
///
/// The classifier charges the conservative number of LCIR aggregate
/// instructions each place use can create. Crossing this boundary selects one
/// atomic unsupported result before any target-typed SSA is allocated.
pub(crate) const PLACE_MAX_AGGREGATE_WORK: usize = 65_536;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PlaceUse {
    Read,
    Move,
    Write,
    InOut,
}

impl PlaceUse {
    pub(crate) fn aggregate_work(self, depth: usize) -> Option<usize> {
        if depth == 0 {
            return Some(0);
        }
        match self {
            Self::Read | Self::Move => Some(depth),
            Self::Write => depth.checked_mul(2)?.checked_sub(1),
            // One read plus reconstruction on both the normal and fault
            // edges. Infallible calls use less, but the preflight deliberately
            // stays independent of the later transitive-effect fixed point.
            Self::InOut => depth.checked_mul(5)?.checked_sub(2),
        }
    }
}

/// Artifact-wide preflight accounting for aggregate place operations.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PlaceBudget {
    aggregate_work: usize,
}

impl PlaceBudget {
    pub(crate) fn admit(&mut self, usage: PlaceUse, depth: usize) -> bool {
        if depth > PLACE_MAX_PROJECTION_DEPTH {
            return false;
        }
        let Some(work) = usage.aggregate_work(depth) else {
            return false;
        };
        let Some(total) = self.aggregate_work.checked_add(work) else {
            return false;
        };
        if total > PLACE_MAX_AGGREGATE_WORK {
            return false;
        }
        self.aggregate_work = total;
        true
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PlaceStep {
    parent_type: ValueTypeId,
    parent_repr: ReprId,
    field: u32,
    field_type: ValueTypeId,
    field_repr: ReprId,
}

impl PlaceStep {
    pub(crate) const fn parent_type(self) -> ValueTypeId {
        self.parent_type
    }

    pub(crate) const fn parent_repr(self) -> ReprId {
        self.parent_repr
    }

    pub(crate) const fn field(self) -> u32 {
        self.field
    }

    pub(crate) const fn field_type(self) -> ValueTypeId {
        self.field_type
    }

    pub(crate) const fn field_repr(self) -> ReprId {
        self.field_repr
    }
}

/// A target-typed path from one MIR local to an exact physical LCIR field.
///
/// A plan contains no address and allocates no storage. Reads are pure SSA
/// extraction; writes reconstruct parents from the leaf back to the current
/// root. Retaining both semantic value-type and physical representation ids
/// makes accidental layout-compatible type substitution impossible.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlacePlan {
    local: LocalId,
    root_type: ValueTypeId,
    root_repr: ReprId,
    leaf_type: ValueTypeId,
    leaf_repr: ReprId,
    steps: Box<[PlaceStep]>,
}

impl PlacePlan {
    pub(crate) fn build(
        representations: &RepresentationPlan,
        place: &Place,
        root_type: ValueTypeId,
    ) -> Result<Self, PlacePlanError> {
        if place.projection.len() > PLACE_MAX_PROJECTION_DEPTH {
            return Err(PlacePlanError::new(format!(
                "place projection depth {} exceeds the supported limit {}",
                place.projection.len(),
                PLACE_MAX_PROJECTION_DEPTH
            )));
        }
        let root = representations.value_type(root_type).ok_or_else(|| {
            PlacePlanError::new(format!("place root has unknown LCIR type {root_type}"))
        })?;
        let root_repr = root.repr();
        let mut current_type = root_type;
        let mut current_repr = root_repr;
        let mut steps = Vec::new();
        steps
            .try_reserve_exact(place.projection.len())
            .map_err(|error| {
                PlacePlanError::new(format!("cannot allocate typed place plan: {error}"))
            })?;
        for (index, field) in place.projection.iter().copied().enumerate() {
            let parent = representations.value_type(current_type).ok_or_else(|| {
                PlacePlanError::new(format!(
                    "place projection {index} has unknown parent type {current_type}"
                ))
            })?;
            if parent.kind() != ValueTypeKind::Direct {
                return Err(PlacePlanError::new(format!(
                    "place projection {index} crosses protected value type {current_type}"
                )));
            }
            if parent.repr() != current_repr {
                return Err(PlacePlanError::new(format!(
                    "place projection {index} changed physical parent identity"
                )));
            }
            let Repr::Product(product) =
                representations.repr(current_repr).copied().ok_or_else(|| {
                    PlacePlanError::new(format!(
                        "place projection {index} has unknown representation {current_repr}"
                    ))
                })?
            else {
                return Err(PlacePlanError::new(format!(
                    "place projection {index} targets non-product representation {current_repr}"
                )));
            };
            let field_type = representations
                .product(product)
                .and_then(|product| {
                    usize::try_from(field)
                        .ok()
                        .and_then(|i| product.fields().get(i))
                })
                .copied()
                .ok_or_else(|| {
                    PlacePlanError::new(format!(
                        "place projection {index} field {field} is outside {current_type}"
                    ))
                })?;
            let field_repr = representations
                .value_type(field_type)
                .map(crate::ValueType::repr)
                .ok_or_else(|| {
                    PlacePlanError::new(format!(
                        "place projection {index} field has unknown LCIR type {field_type}"
                    ))
                })?;
            steps.push(PlaceStep {
                parent_type: current_type,
                parent_repr: current_repr,
                field,
                field_type,
                field_repr,
            });
            current_type = field_type;
            current_repr = field_repr;
        }
        Ok(Self {
            local: place.local,
            root_type,
            root_repr,
            leaf_type: current_type,
            leaf_repr: current_repr,
            steps: steps.into_boxed_slice(),
        })
    }

    pub(crate) const fn local(&self) -> LocalId {
        self.local
    }

    pub(crate) const fn root_type(&self) -> ValueTypeId {
        self.root_type
    }

    pub(crate) const fn root_repr(&self) -> ReprId {
        self.root_repr
    }

    pub(crate) const fn leaf_type(&self) -> ValueTypeId {
        self.leaf_type
    }

    pub(crate) const fn leaf_repr(&self) -> ReprId {
        self.leaf_repr
    }

    pub(crate) fn steps(&self) -> &[PlaceStep] {
        &self.steps
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlacePlanError {
    message: String,
}

impl PlacePlanError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for PlacePlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for PlacePlanError {}
