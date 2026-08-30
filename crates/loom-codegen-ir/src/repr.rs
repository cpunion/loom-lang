use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

use loom_mir::Type;

use crate::ids::ProgramBrand;
use crate::{ProductReprId, ReprId, SumReprId, ValueTypeId};

pub(crate) const DIRECT_PRODUCT_MAX_NESTING_DEPTH: usize = 256;
pub(crate) const DIRECT_PRODUCT_MAX_STRUCTURAL_NODES: usize = 256;

/// Returns whether a nominal semantic identity is fully instantiated and
/// small enough to retain in the direct representation catalog.
pub(crate) fn is_concrete_nominal_type(root: &Type) -> bool {
    if !matches!(root, Type::Nominal(_, _)) {
        return false;
    }
    let mut pending = vec![root];
    let mut visited = 0_usize;
    while let Some(ty) = pending.pop() {
        visited = visited.saturating_add(1);
        if visited > DIRECT_PRODUCT_MAX_STRUCTURAL_NODES {
            return false;
        }
        match ty {
            Type::Tuple(elements) | Type::Nominal(_, elements) => {
                if visited
                    .checked_add(pending.len())
                    .and_then(|nodes| nodes.checked_add(elements.len()))
                    .is_none_or(|nodes| nodes > DIRECT_PRODUCT_MAX_STRUCTURAL_NODES)
                {
                    return false;
                }
                pending.extend(elements);
            }
            Type::List(element) | Type::Task(element) | Type::TaskOutcome(element) => {
                if visited
                    .checked_add(pending.len())
                    .is_none_or(|nodes| nodes >= DIRECT_PRODUCT_MAX_STRUCTURAL_NODES)
                {
                    return false;
                }
                pending.push(element);
            }
            Type::View { bindings, .. } => {
                if visited
                    .checked_add(pending.len())
                    .and_then(|nodes| nodes.checked_add(bindings.len()))
                    .is_none_or(|nodes| nodes > DIRECT_PRODUCT_MAX_STRUCTURAL_NODES)
                {
                    return false;
                }
                pending.extend(bindings.values());
            }
            Type::Parameter(_) | Type::AssociatedProjection { .. } | Type::Error => return false,
            Type::Never | Type::Unit | Type::Bool | Type::Int | Type::Float | Type::Text => {}
        }
    }
    true
}

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
/// This vocabulary grows only alongside complete lowering and validation
/// rules. `ImmortalText` is deliberately narrower than a managed reference:
/// admitted values can originate only in compiler-emitted literal objects, so
/// no moving-GC root is required. `ManagedPointer` is one precisely rooted,
/// direct managed-object base pointer; this slice registers it for dynamic
/// Text, canonical immutable Bytes, and concrete managed collection values.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Repr {
    /// Control-flow vocabulary for semantic `Never`. The direct foundation
    /// does not permit this representation in a function signature or SSA
    /// value; noreturn operations must be modeled by dedicated terminators.
    Uninhabited,
    Zst,
    Scalar(ScalarRepr),
    /// One opaque pointer to a process-lifetime Text object emitted by the
    /// compiler. It is not the representation of dynamically allocated Text.
    ImmortalText,
    /// One exact object-base pointer understood by the typed moving collector.
    /// Static immortal objects are valid values of this representation too.
    ManagedPointer,
    /// One scheduler-owned structured Task handle. The pointee is stable and
    /// never belongs to the moving heap, so this representation is excluded
    /// from typed GC root maps. Values can be created only by `TaskCreate`,
    /// `IoTaskCreate`, `TaskJoin`, or `TaskJoinList`.
    /// Ordinary handles are consumed by an async suspension terminator;
    /// terminal handles injected by `settled` or `race` are consumed by
    /// `TaskOutcomeTake` immediately after resumption.
    TaskHandle,
    /// An immutable register aggregate whose ordered fields are independently
    /// typed LCIR values. Closed products may contain other products and exact
    /// managed-pointer leaves; validation rejects immortal-only Text leaves,
    /// missing, uninhabited, or cyclic field graphs.
    Product(ProductReprId),
    /// A closed tagged union whose ordered variants carry independently typed
    /// payload fields. The sum plan fixes tag width and payload shape, while
    /// the target backend selects the exact carrier size and alignment.
    Sum(SumReprId),
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

/// Minimal discriminant representation selected from the closed variant
/// count. A single-variant sum has no observable tag in its physical ABI.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SumTagRepr {
    Tagless,
    I8,
    I16,
    I32,
}

impl SumTagRepr {
    pub(crate) fn for_variant_count(variants: usize) -> Option<Self> {
        match variants {
            0 => None,
            1 => Some(Self::Tagless),
            2..=256 => Some(Self::I8),
            257..=65_536 => Some(Self::I16),
            _ if u32::try_from(variants).is_ok() => Some(Self::I32),
            _ => None,
        }
    }
}

/// Ordered payload fields of one closed sum variant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SumVariantRepr {
    fields: Box<[ValueTypeId]>,
}

impl SumVariantRepr {
    #[must_use]
    pub const fn fields(&self) -> &[ValueTypeId] {
        &self.fields
    }
}

/// Physical plan for one closed concrete sum.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SumRepr {
    tag: SumTagRepr,
    variants: Box<[SumVariantRepr]>,
}

/// Closed compiler-private payload catalog for one first-class dynamic
/// concept value. The value itself is one managed object pointer. Each
/// allocation stores a private ordinal tag followed by exactly one candidate
/// payload, and the backend emits a distinct precise GC descriptor for every
/// candidate layout.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DynamicRepr {
    view: ValueTypeId,
    candidates: Box<[ValueTypeId]>,
}

impl DynamicRepr {
    #[must_use]
    pub const fn view(&self) -> ValueTypeId {
        self.view
    }

    #[must_use]
    pub const fn candidates(&self) -> &[ValueTypeId] {
        &self.candidates
    }
}

impl SumRepr {
    #[must_use]
    pub const fn tag(&self) -> SumTagRepr {
        self.tag
    }

    #[must_use]
    pub const fn variants(&self) -> &[SumVariantRepr] {
        &self.variants
    }

    /// Returns true when every variant has no semantic payload fields. A
    /// multi-variant all-empty sum is represented by its tag alone.
    #[must_use]
    pub fn is_tag_only(&self) -> bool {
        self.variants.len() > 1
            && self
                .variants
                .iter()
                .all(|variant| variant.fields.is_empty())
    }
}

/// A semantic Loom type paired with one selected target representation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValueType {
    semantic: Type,
    repr: ReprId,
    kind: ValueTypeKind,
}

/// The checked semantic relationship which authorized one canonical value
/// representation.
///
/// `Transparent` values retain a distinct nominal [`ValueTypeId`] while
/// sharing their established base value's physical [`ReprId`].
/// `InvariantProduct` values use an ordinary product representation, but may
/// only be created by a dedicated proof-establishing instruction. This also
/// protects compiler-known semantic invariants such as canonical `Path`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValueTypeKind {
    Direct,
    /// Compiler-private single-pointer representation of one closed
    /// `TextMap[V]`. The semantic argument identifies `V`; keeping this marker
    /// separate from ordinary direct values lets independent validation reject
    /// forged nominal managed pointers without introducing source-visible RTTI.
    ManagedTextMap,
    Transparent {
        base: ValueTypeId,
    },
    InvariantProduct,
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

    #[must_use]
    pub const fn kind(&self) -> ValueTypeKind {
        self.kind
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
    sums: Vec<SumRepr>,
    dynamics: Vec<DynamicRepr>,
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
            kind: ValueTypeKind::Direct,
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
            sums: Vec::new(),
            dynamics: Vec::new(),
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
    pub fn sums(&self) -> &[SumRepr] {
        &self.sums
    }

    #[must_use]
    pub fn dynamics(&self) -> &[DynamicRepr] {
        &self.dynamics
    }

    #[must_use]
    pub fn dynamic(&self, view: ValueTypeId) -> Option<&DynamicRepr> {
        self.dynamics.iter().find(|dynamic| dynamic.view == view)
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
    pub fn sum(&self, id: SumReprId) -> Option<&SumRepr> {
        (id.brand() == self.brand)
            .then(|| self.sums.get(id.index()))
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

    /// Returns whether `ty` is the canonical direct managed representation of
    /// the compiler-known immutable Bytes type.
    #[must_use]
    pub fn is_managed_bytes_type(
        &self,
        canonical_bytes: Option<loom_mir::TypeId>,
        ty: ValueTypeId,
    ) -> bool {
        canonical_bytes.and_then(|bytes| self.type_id(&Type::Nominal(bytes, Vec::new())))
            == Some(ty)
            && self.value_type(ty).is_some_and(|value_type| {
                value_type.kind() == ValueTypeKind::Direct
                    && self.repr(value_type.repr()) == Some(&Repr::ManagedPointer)
            })
    }

    /// Returns whether an already checked representation graph contains an
    /// immortal Text and/or managed pointer. Missing or cyclic references are
    /// rejected instead of being guessed; independent validation repeats the
    /// graph rules after construction.
    fn pointer_kinds(&self, root: ValueTypeId) -> Option<(bool, bool)> {
        let mut pending = vec![root];
        let mut visited = BTreeSet::new();
        let mut immortal = false;
        let mut managed = false;
        while let Some(value_id) = pending.pop() {
            if !visited.insert(value_id) {
                continue;
            }
            let value = self.value_type(value_id)?;
            match self.repr(value.repr())? {
                Repr::ImmortalText => immortal = true,
                Repr::ManagedPointer => managed = true,
                Repr::Product(product) => {
                    pending.extend(self.product(*product)?.fields().iter().copied());
                }
                Repr::Sum(sum) => {
                    pending.extend(
                        self.sum(*sum)?
                            .variants()
                            .iter()
                            .flat_map(|variant| variant.fields().iter().copied()),
                    );
                }
                Repr::Uninhabited | Repr::Zst | Repr::Scalar(_) | Repr::TaskHandle => {}
            }
        }
        Some((immortal, managed))
    }

    fn contains_task_handle(&self, root: ValueTypeId) -> Option<bool> {
        let mut pending = vec![root];
        let mut visited = BTreeSet::new();
        while let Some(value) = pending.pop() {
            if !visited.insert(value) {
                continue;
            }
            let value = self.value_type(value)?;
            match self.repr(value.repr())? {
                Repr::TaskHandle => return Some(true),
                Repr::Product(product) => {
                    pending.extend(self.product(*product)?.fields().iter().copied());
                }
                Repr::Sum(sum) => pending.extend(
                    self.sum(*sum)?
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

    fn add_product(
        &mut self,
        semantic: Type,
        fields: &[Type],
        kind: ValueTypeKind,
    ) -> Option<ValueTypeId> {
        if self.type_id(&semantic).is_some() {
            return None;
        }
        if !matches!(
            kind,
            ValueTypeKind::Direct | ValueTypeKind::InvariantProduct
        ) {
            return None;
        }
        let fields = fields
            .iter()
            .map(|field| self.type_id(field))
            .collect::<Option<Vec<_>>>()?;
        // Products may contain managed pointers, but never immortal-only Text.
        // Source classification selects one artifact-wide managed-capable Text
        // representation before registering a Text-bearing product.
        if fields
            .iter()
            .any(|field| self.pointer_kinds(*field).is_none_or(|kinds| kinds.0))
        {
            return None;
        }
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
            kind,
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
        if !is_concrete_nominal_type(&semantic) {
            return None;
        }
        self.add_product(semantic, fields, ValueTypeKind::Direct)
    }

    pub(crate) fn add_immortal_text(&mut self) -> Option<ValueTypeId> {
        if self.target.pointer_bits() != 64 || self.type_id(&Type::Text).is_some() {
            return None;
        }
        let repr = ReprId::from_index(self.brand, self.reprs.len())?;
        let ty = ValueTypeId::from_index(self.brand, self.types.len())?;
        self.reprs.push(Repr::ImmortalText);
        self.types.push(ValueType {
            semantic: Type::Text,
            repr,
            kind: ValueTypeKind::Direct,
        });
        self.registrations.push(TypeRegistration {
            semantic: Type::Text,
            value_type: ty,
        });
        self.canonical_types.insert(Type::Text, ty);
        Some(ty)
    }

    pub(crate) fn add_managed_text(&mut self) -> Option<ValueTypeId> {
        if self.target.pointer_bits() != 64 || self.type_id(&Type::Text).is_some() {
            return None;
        }
        let repr = ReprId::from_index(self.brand, self.reprs.len())?;
        let ty = ValueTypeId::from_index(self.brand, self.types.len())?;
        self.reprs.push(Repr::ManagedPointer);
        self.types.push(ValueType {
            semantic: Type::Text,
            repr,
            kind: ValueTypeKind::Direct,
        });
        self.registrations.push(TypeRegistration {
            semantic: Type::Text,
            value_type: ty,
        });
        self.canonical_types.insert(Type::Text, ty);
        Some(ty)
    }

    pub(crate) fn add_managed_bytes(
        &mut self,
        semantic: Type,
        canonical_bytes: loom_mir::TypeId,
    ) -> Option<ValueTypeId> {
        if self.target.pointer_bits() != 64
            || semantic != Type::Nominal(canonical_bytes, Vec::new())
            || self.type_id(&semantic).is_some()
        {
            return None;
        }
        let repr = ReprId::from_index(self.brand, self.reprs.len())?;
        let ty = ValueTypeId::from_index(self.brand, self.types.len())?;
        self.reprs.push(Repr::ManagedPointer);
        self.types.push(ValueType {
            semantic: semantic.clone(),
            repr,
            kind: ValueTypeKind::Direct,
        });
        self.registrations.push(TypeRegistration {
            semantic: semantic.clone(),
            value_type: ty,
        });
        self.canonical_types.insert(semantic, ty);
        Some(ty)
    }

    pub(crate) fn add_managed_list(&mut self, semantic: Type) -> Option<ValueTypeId> {
        if self.target.pointer_bits() != 64
            || self.type_id(&semantic).is_some()
            || !matches!(semantic, Type::List(_))
        {
            return None;
        }
        let repr = ReprId::from_index(self.brand, self.reprs.len())?;
        let ty = ValueTypeId::from_index(self.brand, self.types.len())?;
        self.reprs.push(Repr::ManagedPointer);
        self.types.push(ValueType {
            semantic: semantic.clone(),
            repr,
            kind: ValueTypeKind::Direct,
        });
        self.registrations.push(TypeRegistration {
            semantic: semantic.clone(),
            value_type: ty,
        });
        self.canonical_types.insert(semantic, ty);
        Some(ty)
    }

    pub(crate) fn add_managed_text_map(
        &mut self,
        semantic: Type,
        canonical_text_map: loom_mir::TypeId,
    ) -> Option<ValueTypeId> {
        let Type::Nominal(identity, arguments) = &semantic else {
            return None;
        };
        if self.target.pointer_bits() != 64
            || self.type_id(&semantic).is_some()
            || *identity != canonical_text_map
            || arguments.len() != 1
        {
            return None;
        }
        let repr = ReprId::from_index(self.brand, self.reprs.len())?;
        let ty = ValueTypeId::from_index(self.brand, self.types.len())?;
        self.reprs.push(Repr::ManagedPointer);
        self.types.push(ValueType {
            semantic: semantic.clone(),
            repr,
            kind: ValueTypeKind::ManagedTextMap,
        });
        self.registrations.push(TypeRegistration {
            semantic: semantic.clone(),
            value_type: ty,
        });
        self.canonical_types.insert(semantic, ty);
        Some(ty)
    }

    pub(crate) fn add_managed_dynamic(
        &mut self,
        semantic: Type,
        candidates: &[Type],
    ) -> Option<ValueTypeId> {
        if self.target.pointer_bits() != 64
            || self.type_id(&semantic).is_some()
            || !matches!(semantic, Type::View { .. })
            || candidates.len() < 2
        {
            return None;
        }
        let candidates = candidates
            .iter()
            .map(|candidate| self.type_id(candidate))
            .collect::<Option<Vec<_>>>()?;
        if candidates.iter().copied().collect::<BTreeSet<_>>().len() != candidates.len()
            || candidates.iter().any(|candidate| {
                self.value_type(*candidate).is_none_or(|value_type| {
                    value_type.semantic() == &Type::Never
                        || matches!(self.repr(value_type.repr()), Some(Repr::Uninhabited))
                        || self.contains_task_handle(*candidate) != Some(false)
                        || self
                            .pointer_kinds(*candidate)
                            .is_none_or(|(immortal, _)| immortal)
                })
            })
        {
            return None;
        }
        let repr = ReprId::from_index(self.brand, self.reprs.len())?;
        let ty = ValueTypeId::from_index(self.brand, self.types.len())?;
        self.reprs.push(Repr::ManagedPointer);
        self.types.push(ValueType {
            semantic: semantic.clone(),
            repr,
            kind: ValueTypeKind::Direct,
        });
        self.registrations.push(TypeRegistration {
            semantic: semantic.clone(),
            value_type: ty,
        });
        self.canonical_types.insert(semantic, ty);
        self.dynamics.push(DynamicRepr {
            view: ty,
            candidates: candidates.into_boxed_slice(),
        });
        Some(ty)
    }

    pub(crate) fn add_task_handle(&mut self, semantic: Type) -> Option<ValueTypeId> {
        if self.target.pointer_bits() != 64
            || self.type_id(&semantic).is_some()
            || !matches!(&semantic, Type::Task(output) if self.type_id(output).is_some())
        {
            return None;
        }
        let repr = ReprId::from_index(self.brand, self.reprs.len())?;
        let ty = ValueTypeId::from_index(self.brand, self.types.len())?;
        self.reprs.push(Repr::TaskHandle);
        self.types.push(ValueType {
            semantic: semantic.clone(),
            repr,
            kind: ValueTypeKind::Direct,
        });
        self.registrations.push(TypeRegistration {
            semantic: semantic.clone(),
            value_type: ty,
        });
        self.canonical_types.insert(semantic, ty);
        Some(ty)
    }

    pub(crate) fn add_invariant_record(
        &mut self,
        semantic: Type,
        fields: &[Type],
    ) -> Option<ValueTypeId> {
        if !is_concrete_nominal_type(&semantic) {
            return None;
        }
        self.add_product(semantic, fields, ValueTypeKind::InvariantProduct)
    }

    pub(crate) fn add_tuple(&mut self, elements: &[Type]) -> Option<ValueTypeId> {
        self.add_product(
            Type::Tuple(elements.to_vec()),
            elements,
            ValueTypeKind::Direct,
        )
    }

    pub(crate) fn add_transparent(&mut self, semantic: Type, base: &Type) -> Option<ValueTypeId> {
        if self.type_id(&semantic).is_some() || !is_concrete_nominal_type(&semantic) {
            return None;
        }
        let base = self.type_id(base)?;
        let repr = self.value_type(base)?.repr;
        if matches!(self.repr(repr), Some(Repr::Uninhabited))
            || self
                .pointer_kinds(base)
                .is_none_or(|(immortal, _)| immortal)
        {
            return None;
        }
        let ty = ValueTypeId::from_index(self.brand, self.types.len())?;
        self.types.push(ValueType {
            semantic: semantic.clone(),
            repr,
            kind: ValueTypeKind::Transparent { base },
        });
        self.registrations.push(TypeRegistration {
            semantic: semantic.clone(),
            value_type: ty,
        });
        self.canonical_types.insert(semantic, ty);
        Some(ty)
    }

    pub(crate) fn add_sum(
        &mut self,
        semantic: Type,
        variants: &[Box<[Type]>],
    ) -> Option<ValueTypeId> {
        if self.type_id(&semantic).is_some() || !matches!(&semantic, Type::Nominal(_, _)) {
            return None;
        }
        let tag = SumTagRepr::for_variant_count(variants.len())?;
        let variants = variants
            .iter()
            .map(|variant| {
                variant
                    .iter()
                    .map(|field| self.type_id(field))
                    .collect::<Option<Vec<_>>>()
                    .map(|fields| SumVariantRepr {
                        fields: fields.into_boxed_slice(),
                    })
            })
            .collect::<Option<Vec<_>>>()?;
        // Closed sums may contain exact managed or affine leaves, including
        // through nested products and sums. Immortal-only Text remains
        // excluded: sum construction, tag flow, and payload projection do not
        // carry its separate provenance proof.
        if variants.iter().any(|variant| {
            variant.fields.iter().any(|field| {
                self.pointer_kinds(*field)
                    .is_none_or(|(immortal, _)| immortal)
            })
        }) {
            return None;
        }
        let sum = SumReprId::from_index(self.brand, self.sums.len())?;
        let repr = ReprId::from_index(self.brand, self.reprs.len())?;
        let ty = ValueTypeId::from_index(self.brand, self.types.len())?;
        self.sums.push(SumRepr {
            tag,
            variants: variants.into_boxed_slice(),
        });
        self.reprs.push(Repr::Sum(sum));
        self.types.push(ValueType {
            semantic: semantic.clone(),
            repr,
            kind: ValueTypeKind::Direct,
        });
        self.registrations.push(TypeRegistration {
            semantic: semantic.clone(),
            value_type: ty,
        });
        self.canonical_types.insert(semantic, ty);
        Some(ty)
    }
}

#[cfg(test)]
mod tests {
    use loom_mir::{ConceptId, FunctionId as MirFunctionId, TypeId};

    use super::*;
    use crate::{
        Constant, CoroutinePlan, Effects, InstructionKind, Origin, Program, ProgramBuilder,
        ResourceKind, Signature, Terminator, TerminatorKind, ValidationCode, ValueDefinition,
        validate_program,
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

    fn canonical_file_close_program() -> (Program, ValueTypeId, ValueTypeId) {
        let origin = Origin::synthetic(MirFunctionId(93));
        let file_id = TypeId(107);
        let mut builder = ProgramBuilder::with_canonical_types(
            TargetLayout::new(64).expect("target"),
            crate::CanonicalTypeCatalog {
                file: Some(file_id),
                ..crate::CanonicalTypeCatalog::default()
            },
        );
        let file_semantic = Type::Nominal(file_id, Vec::new());
        let file = builder
            .add_pod_record_type(file_semantic, &[Type::Int])
            .expect("canonical File");
        let unit = builder.type_id(&Type::Unit).expect("Unit");
        let function = builder
            .declare_function(
                origin,
                "resource.close.noncanonical",
                Signature::new([file], unit),
                Effects::NEEDS_EXECUTOR.with_implications(),
            )
            .expect("declare resource close");
        {
            let mut function = builder.function(function).expect("function builder");
            let entry = function.create_block().expect("entry");
            function.set_entry(entry).expect("set entry");
            let resource = function
                .append_block_parameter(entry, file)
                .expect("File parameter");
            let results = function
                .append_instruction(
                    entry,
                    InstructionKind::ResourceClose {
                        kind: ResourceKind::File,
                        resource,
                    },
                    &[unit, file],
                    origin,
                )
                .expect("resource close");
            let returned = results[0];
            function
                .terminate(
                    entry,
                    Terminator::new(TerminatorKind::Return(returned), origin),
                )
                .expect("return");
        }

        let program = builder.finish();
        validate_program(&program).expect("canonical File cleanup");
        (program, file, unit)
    }

    fn assert_file_close_type_mismatch(program: &Program, context: &str) {
        let errors = validate_program(program).expect_err(context);
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::TypeMismatch
                && error.path() == "function[0].instruction[0].resource"
                && error.message().contains("canonical File")
        }));
    }

    #[test]
    fn resource_close_rejects_an_unregistered_file_value_type() {
        let (program, file, unit) = canonical_file_close_program();
        let mut alternative_resource = program.clone();
        let alternative = ValueTypeId::from_index(
            alternative_resource.brand,
            alternative_resource.representations.types.len(),
        )
        .expect("alternative value type identity");
        let canonical = alternative_resource.representations.types[file.index()].clone();
        alternative_resource.representations.types.push(canonical);
        alternative_resource.functions[0].signature = Signature::new([alternative], unit);
        for value in [0_usize, 2] {
            alternative_resource.functions[0].values[value].ty = alternative;
        }
        assert_file_close_type_mismatch(
            &alternative_resource,
            "ResourceClose must reject a noncanonical File ValueType",
        );
    }

    #[test]
    fn resource_close_rejects_a_duplicate_file_registration() {
        let (program, _, _) = canonical_file_close_program();
        let mut duplicate_registration = program.clone();
        let file_id = duplicate_registration
            .canonical_types
            .file
            .expect("canonical File identity");
        let duplicate = duplicate_registration
            .representations
            .registrations
            .iter()
            .find(|registration| registration.semantic == Type::Nominal(file_id, Vec::new()))
            .cloned()
            .expect("File registration");
        duplicate_registration
            .representations
            .registrations
            .push(duplicate);
        assert_file_close_type_mismatch(
            &duplicate_registration,
            "ResourceClose must reject duplicate File registrations",
        );
    }

    #[test]
    fn resource_close_rejects_an_unregistered_int_field_value_type() {
        let (program, file, _) = canonical_file_close_program();
        let mut alternative_field = program;
        let integer = alternative_field
            .representations
            .type_id(&Type::Int)
            .expect("canonical Int");
        let alternative_integer = ValueTypeId::from_index(
            alternative_field.brand,
            alternative_field.representations.types.len(),
        )
        .expect("alternative Int value type identity");
        let integer_value = alternative_field.representations.types[integer.index()].clone();
        alternative_field.representations.types.push(integer_value);
        let file_repr = alternative_field.representations.types[file.index()].repr;
        let Repr::Product(file_product) =
            alternative_field.representations.reprs[file_repr.index()]
        else {
            panic!("File must retain a product representation")
        };
        alternative_field.representations.products[file_product.index()].fields[0] =
            alternative_integer;
        assert_file_close_type_mismatch(
            &alternative_field,
            "ResourceClose must reject a noncanonical Int field ValueType",
        );
    }

    #[test]
    fn coroutine_frames_reject_noncanonical_managed_collection_alternatives() {
        let mut builder = ProgramBuilder::with_canonical_types(
            TargetLayout::new(64).expect("target"),
            crate::CanonicalTypeCatalog {
                text_map: Some(TypeId(96)),
                ..crate::CanonicalTypeCatalog::default()
            },
        );
        builder
            .add_managed_text_type()
            .expect("register managed Text");
        let list_semantic = Type::List(Box::new(Type::Text));
        let list = builder
            .add_managed_list_type(list_semantic.clone())
            .expect("List[Text]");
        let map_semantic = Type::Nominal(TypeId(96), vec![list_semantic]);
        let map = builder
            .add_managed_text_map_type(map_semantic)
            .expect("TextMap[List[Text]]");
        let unit = builder.type_id(&Type::Unit).expect("Unit");

        for (index, (name, parameter)) in [("noncanonical.list", list), ("noncanonical.map", map)]
            .into_iter()
            .enumerate()
        {
            let origin = Origin::synthetic(MirFunctionId(
                96 + u32::try_from(index).expect("fixture function index"),
            ));
            let function = builder
                .declare_function(
                    origin,
                    name,
                    Signature::new([parameter], unit),
                    Effects::NEEDS_EXECUTOR.with_implications(),
                )
                .expect("declare coroutine");
            let mut function = builder.function(function).expect("function builder");
            function
                .set_coroutine_plan(CoroutinePlan::new(unit, []))
                .expect("coroutine plan");
            let entry = function.create_block().expect("entry");
            function.set_entry(entry).expect("set entry");
            function
                .append_block_parameter(entry, parameter)
                .expect("collection parameter");
            let result = function
                .append_instruction(
                    entry,
                    InstructionKind::Constant(Constant::Unit),
                    &[unit],
                    origin,
                )
                .expect("Unit")[0];
            function
                .terminate(
                    entry,
                    Terminator::new(TerminatorKind::Return(result), origin),
                )
                .expect("return");
        }

        let mut program = builder.finish();
        for (function_index, canonical) in [list, map].into_iter().enumerate() {
            let canonical = program.representations.types[canonical.index()].clone();
            let alternative_repr =
                ReprId::from_index(program.brand, program.representations.reprs.len())
                    .expect("alternative representation identity");
            let alternative_type =
                ValueTypeId::from_index(program.brand, program.representations.types.len())
                    .expect("alternative value type identity");
            program.representations.reprs.push(Repr::ManagedPointer);
            program.representations.types.push(ValueType {
                semantic: canonical.semantic,
                repr: alternative_repr,
                kind: canonical.kind,
            });
            program.functions[function_index].signature = Signature::new([alternative_type], unit);
            program.functions[function_index].values[0].ty = alternative_type;
        }

        let errors = validate_program(&program)
            .expect_err("coroutine frames must reject noncanonical collection representations");
        for function_index in 0..2 {
            assert!(
                errors.as_slice().iter().any(|error| {
                    error.code() == ValidationCode::InvalidCoroutinePlan
                        && error.path()
                            == format!("function[{function_index}].coroutine.frame_type[0]")
                }),
                "{errors:#?}"
            );
        }
    }

    #[test]
    fn text_utf8_units_reject_a_noncanonical_managed_list_alternative() {
        let origin = Origin::synthetic(MirFunctionId(98));
        let result_id = TypeId(101);
        let decode_error_id = TypeId(111);
        let mut builder = ProgramBuilder::with_canonical_types(
            TargetLayout::new(64).expect("target"),
            crate::CanonicalTypeCatalog {
                result: Some(result_id),
                decode_text_error: Some(decode_error_id),
                ..crate::CanonicalTypeCatalog::default()
            },
        );
        builder
            .add_managed_text_type()
            .expect("canonical managed Text");
        let list_semantic = Type::List(Box::new(Type::Int));
        let list = builder
            .add_managed_list_type(list_semantic)
            .expect("canonical List[Int]");
        let decode_error_semantic = Type::Nominal(decode_error_id, Vec::new());
        builder
            .add_sum_type(decode_error_semantic.clone(), &[Box::new([])])
            .expect("DecodeTextError");
        let decode_result = builder
            .add_sum_type(
                Type::Nominal(result_id, vec![Type::Text, decode_error_semantic.clone()]),
                &[Box::new([Type::Text]), Box::from([decode_error_semantic])],
            )
            .expect("Result[Text, DecodeTextError]");
        let unit = builder.type_id(&Type::Unit).expect("Unit");
        let root = builder
            .declare_function(
                origin,
                "text.units.noncanonical",
                Signature::new([list], unit),
                Effects::MAY_COLLECT.with_implications(),
            )
            .expect("function");
        {
            let mut function = builder.function(root).expect("function builder");
            let entry = function.create_block().expect("entry");
            function.set_entry(entry).expect("set entry");
            let units = function
                .append_block_parameter(entry, list)
                .expect("List[Int] parameter");
            function
                .append_instruction(
                    entry,
                    InstructionKind::TextFromUtf8Units {
                        units,
                        ok_variant: 0,
                        error_variant: 1,
                        invalid_utf8_variant: 0,
                    },
                    &[decode_result],
                    origin,
                )
                .expect("Text construction");
            let result = function
                .append_instruction(
                    entry,
                    InstructionKind::Constant(Constant::Unit),
                    &[unit],
                    origin,
                )
                .expect("Unit")[0];
            function
                .terminate(
                    entry,
                    Terminator::new(TerminatorKind::Return(result), origin),
                )
                .expect("return");
        }

        let mut program = builder.finish();
        validate_program(&program).expect("canonical Text construction");
        let canonical = program.representations.types[list.index()].clone();
        let alternative_repr =
            ReprId::from_index(program.brand, program.representations.reprs.len())
                .expect("alternative representation identity");
        let alternative =
            ValueTypeId::from_index(program.brand, program.representations.types.len())
                .expect("alternative value type identity");
        program.representations.reprs.push(Repr::ManagedPointer);
        program.representations.types.push(ValueType {
            semantic: canonical.semantic,
            repr: alternative_repr,
            kind: canonical.kind,
        });
        program.functions[0].signature = Signature::new([alternative], unit);
        program.functions[0].values[0].ty = alternative;

        let errors = validate_program(&program)
            .expect_err("noncanonical List[Int] must not cross the Text construction boundary");
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::TypeMismatch
                && error.message().contains("canonical List[Int]")
        }));
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one hostile artifact mutates both Path and nested Result representations atomically"
    )]
    fn path_opcodes_reject_noncanonical_path_and_result_alternatives() {
        let origin = Origin::synthetic(MirFunctionId(99));
        let result_id = TypeId(101);
        let path_id = TypeId(110);
        let path_error_id = TypeId(112);
        let catalog = crate::CanonicalTypeCatalog {
            result: Some(result_id),
            path: Some(path_id),
            path_error: Some(path_error_id),
            ..crate::CanonicalTypeCatalog::default()
        };
        let mut builder =
            ProgramBuilder::with_canonical_types(TargetLayout::new(64).expect("target"), catalog);
        let text = builder
            .add_managed_text_type()
            .expect("canonical managed Text");
        let path_semantic = Type::Nominal(path_id, Vec::new());
        let path = builder
            .add_invariant_record_type(path_semantic.clone(), &[Type::Text])
            .expect("Path");
        let error_semantic = Type::Nominal(path_error_id, Vec::new());
        builder
            .add_sum_type(error_semantic.clone(), &[Box::new([]), Box::new([])])
            .expect("PathError");
        let result_semantic = Type::Nominal(
            result_id,
            vec![path_semantic.clone(), error_semantic.clone()],
        );
        let result = builder
            .add_sum_type(
                result_semantic,
                &[Box::from([path_semantic]), Box::from([error_semantic])],
            )
            .expect("Result[Path, PathError]");
        let unit = builder.type_id(&Type::Unit).expect("Unit");
        let root = builder
            .declare_function(
                origin,
                "path.noncanonical",
                Signature::new([path], unit),
                Effects::MAY_COLLECT.with_implications(),
            )
            .expect("function");
        let (path_value, from_value, join_value) = {
            let mut function = builder.function(root).expect("function builder");
            let entry = function.create_block().expect("entry");
            function.set_entry(entry).expect("set entry");
            let path_value = function
                .append_block_parameter(entry, path)
                .expect("Path parameter");
            let rendered = function
                .append_instruction(
                    entry,
                    InstructionKind::PathAsText { path: path_value },
                    &[text],
                    origin,
                )
                .expect("Path.as_text")[0];
            let from_value = function
                .append_instruction(
                    entry,
                    InstructionKind::PathFromText {
                        text: rendered,
                        ok_variant: 0,
                        error_variant: 1,
                        contains_nul_variant: 0,
                    },
                    &[result],
                    origin,
                )
                .expect("Path.from_text")[0];
            let join_value = function
                .append_instruction(
                    entry,
                    InstructionKind::PathJoin {
                        base: path_value,
                        child: path_value,
                        ok_variant: 0,
                        error_variant: 1,
                        absolute_join_variant: 1,
                    },
                    &[result],
                    origin,
                )
                .expect("Path.join")[0];
            let returned = function
                .append_instruction(
                    entry,
                    InstructionKind::Constant(Constant::Unit),
                    &[unit],
                    origin,
                )
                .expect("Unit")[0];
            function
                .terminate(
                    entry,
                    Terminator::new(TerminatorKind::Return(returned), origin),
                )
                .expect("return");
            (path_value, from_value, join_value)
        };

        let mut program = builder.finish();
        validate_program(&program).expect("canonical Path operations");

        let canonical_path = program.representations.types[path.index()].clone();
        let Repr::Product(path_product) =
            program.representations.reprs[canonical_path.repr.index()]
        else {
            panic!("Path must be a product")
        };
        let alternative_product =
            ProductReprId::from_index(program.brand, program.representations.products.len())
                .expect("alternative Path product identity");
        program
            .representations
            .products
            .push(program.representations.products[path_product.index()].clone());
        let alternative_path_repr =
            ReprId::from_index(program.brand, program.representations.reprs.len())
                .expect("alternative Path repr identity");
        program
            .representations
            .reprs
            .push(Repr::Product(alternative_product));
        let alternative_path =
            ValueTypeId::from_index(program.brand, program.representations.types.len())
                .expect("alternative Path type identity");
        program.representations.types.push(ValueType {
            semantic: canonical_path.semantic,
            repr: alternative_path_repr,
            kind: canonical_path.kind,
        });

        let canonical_result = program.representations.types[result.index()].clone();
        let Repr::Sum(result_sum) = program.representations.reprs[canonical_result.repr.index()]
        else {
            panic!("Result must be a sum")
        };
        let alternative_sum =
            SumReprId::from_index(program.brand, program.representations.sums.len())
                .expect("alternative Result sum identity");
        program
            .representations
            .sums
            .push(program.representations.sums[result_sum.index()].clone());
        let alternative_result_repr =
            ReprId::from_index(program.brand, program.representations.reprs.len())
                .expect("alternative Result repr identity");
        program
            .representations
            .reprs
            .push(Repr::Sum(alternative_sum));
        let alternative_result =
            ValueTypeId::from_index(program.brand, program.representations.types.len())
                .expect("alternative Result type identity");
        program.representations.types.push(ValueType {
            semantic: canonical_result.semantic,
            repr: alternative_result_repr,
            kind: canonical_result.kind,
        });

        let function = &mut program.functions[root.index()];
        function.signature = Signature::new([alternative_path], unit);
        function.values[path_value.index()].ty = alternative_path;
        function.values[from_value.index()].ty = alternative_result;
        function.values[join_value.index()].ty = alternative_result;

        let errors = validate_program(&program)
            .expect_err("noncanonical Path and Result alternatives must fail closed");
        assert!(
            errors.as_slice().iter().any(|error| {
                error.code() == ValidationCode::TypeMismatch
                    && (error.path() == "function[0].instruction[0].path"
                        || error.path() == "function[0].instruction[2].base")
            }),
            "{errors:#?}"
        );
        assert!(
            errors.as_slice().iter().any(|error| {
                error.code() == ValidationCode::TypeMismatch
                    && error
                        .message()
                        .contains("cataloged canonical Result[Path, PathError]")
            }),
            "{errors:#?}"
        );
    }

    #[test]
    fn independent_validation_enforces_text_container_boundaries() {
        let mut immortal = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let text = immortal.add_immortal_text_type().expect("immortal Text");
        let tuple = immortal
            .add_tuple_type(&[Type::Int])
            .expect("pointer-free product");
        let mut immortal = immortal.finish();
        let Repr::Product(product) = immortal.representations.reprs
            [immortal.representations.types[tuple.index()].repr.index()]
        else {
            panic!("tuple must use a product")
        };
        immortal.representations.products[product.index()].fields[0] = text;
        let errors = validate_program(&immortal)
            .expect_err("an immortal-only Text pointer cannot be forged into a product");
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::RepresentationPlan
                && error.path() == "representations.product[0].field[0]"
                && error.message()
                    == "product fields must reference inhabited direct values; Text leaves require ManagedPointer"
        }));

        let mut task_aggregate = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let task_semantic = Type::Task(Box::new(Type::Int));
        task_aggregate
            .add_task_handle_type(task_semantic.clone())
            .expect("Task[Int]");
        task_aggregate
            .add_tuple_type(std::slice::from_ref(&task_semantic))
            .expect("affine product");
        task_aggregate
            .add_sum_type(
                Type::Nominal(TypeId(5_001), Vec::new()),
                &[Box::from([task_semantic])],
            )
            .expect("affine sum");
        validate_program(&task_aggregate.finish())
            .expect("by-value products and sums may carry exact Task handles");

        let mut immortal_sum = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let text = immortal_sum
            .add_immortal_text_type()
            .expect("immortal Text");
        immortal_sum
            .add_sum_type(
                Type::Nominal(TypeId(5_000), Vec::new()),
                &[Box::from([Type::Int])],
            )
            .expect("pointer-free sum");
        let mut immortal_sum = immortal_sum.finish();
        immortal_sum.representations.sums[0].variants[0].fields[0] = text;
        let errors = validate_program(&immortal_sum)
            .expect_err("an immortal-only Text pointer cannot be forged into a sum");
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::RepresentationPlan
                && error.path() == "representations.sum[0].variant[0].field[0]"
                && error.message()
                    == "sum payloads must reference inhabited direct values; Text leaves require ManagedPointer"
        }));

        let mut transparent = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        transparent.add_managed_text_type().expect("managed Text");
        let product_semantic = Type::Tuple(vec![Type::Text]);
        transparent
            .add_tuple_type(&[Type::Text])
            .expect("managed product");
        transparent
            .add_transparent_type(Type::Nominal(TypeId(5_001), Vec::new()), &product_semantic)
            .expect("managed transparent value");
        validate_program(&transparent.finish())
            .expect("a transparent carrier may retain exact managed roots from its base");

        let mut immortal_transparent = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let text = immortal_transparent
            .add_immortal_text_type()
            .expect("immortal Text");
        let wrapper = immortal_transparent
            .add_transparent_type(Type::Nominal(TypeId(5_002), Vec::new()), &Type::Float)
            .expect("pointer-free transparent value");
        let mut immortal_transparent = immortal_transparent.finish();
        let text_repr = immortal_transparent.representations.types[text.index()].repr;
        immortal_transparent.representations.types[wrapper.index()].kind =
            ValueTypeKind::Transparent { base: text };
        immortal_transparent.representations.types[wrapper.index()].repr = text_repr;
        let errors = validate_program(&immortal_transparent)
            .expect_err("an immortal-only Text cannot be forged into a transparent carrier");
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::RepresentationPlan
                && error.path()
                    == format!(
                        "representations.type[{}].kind.transparent_base",
                        wrapper.index()
                    )
                && error.message()
                    == "transparent values cannot retain an immortal-only Text base representation"
        }));
    }

    #[test]
    fn dynamic_catalogs_cannot_hide_task_obligations() {
        let view = Type::View {
            mutable: false,
            concept: ConceptId(77),
            bindings: BTreeMap::new(),
        };
        let mut rejected = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        rejected
            .add_task_handle_type(Type::Task(Box::new(Type::Int)))
            .expect("Task[Int]");
        rejected
            .add_managed_dynamic_type(view.clone(), &[Type::Task(Box::new(Type::Int)), Type::Bool])
            .expect_err("a managed dynamic catalog cannot accept a Task candidate");

        let mut forged = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let task = forged
            .add_task_handle_type(Type::Task(Box::new(Type::Int)))
            .expect("Task[Int]");
        forged
            .add_managed_dynamic_type(view, &[Type::Int, Type::Bool])
            .expect("ordinary closed dynamic catalog");
        let mut forged = forged.finish();
        forged.representations.dynamics[0].candidates[0] = task;
        let errors = validate_program(&forged)
            .expect_err("independent validation must reject a forged Task candidate");
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::RepresentationPlan
                && error.path() == "representations.dynamic[0].candidate[0]"
                && error.message().contains("Task handles")
        }));
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
            kind: ValueTypeKind::Direct,
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
    #[allow(clippy::too_many_lines)]
    fn task_creation_rejects_a_noncanonical_coroutine_output_representation() {
        let semantic = Type::Nominal(TypeId(71), Vec::new());
        let child_origin = Origin::synthetic(MirFunctionId(93));
        let root_origin = Origin::synthetic(MirFunctionId(94));
        let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let integer = builder.type_id(&Type::Int).expect("Int");
        let boolean = builder.type_id(&Type::Bool).expect("Bool");
        let unit = builder.type_id(&Type::Unit).expect("Unit");
        let canonical = builder
            .add_pod_record_type(semantic.clone(), &[Type::Int])
            .expect("canonical record");
        let task = builder
            .add_task_handle_type(Type::Task(Box::new(semantic.clone())))
            .expect("Task[record]");

        let child = builder
            .declare_function(
                child_origin,
                "alternative_task.child",
                Signature::new([], canonical),
                Effects::NEEDS_EXECUTOR.with_implications(),
            )
            .expect("child");
        let (integer_value, boolean_value, record_value) = {
            let mut function = builder.function(child).expect("child builder");
            function
                .set_coroutine_plan(CoroutinePlan::new(canonical, []))
                .expect("child coroutine");
            let entry = function.create_block().expect("child entry");
            function.set_entry(entry).expect("set child entry");
            let integer_value = function
                .append_instruction(
                    entry,
                    InstructionKind::Constant(Constant::Int(1)),
                    &[integer],
                    child_origin,
                )
                .expect("Int")[0];
            let boolean_value = function
                .append_instruction(
                    entry,
                    InstructionKind::Constant(Constant::Bool(true)),
                    &[boolean],
                    child_origin,
                )
                .expect("Bool")[0];
            let record_value = function
                .append_instruction(
                    entry,
                    InstructionKind::ProductConstruct {
                        fields: Box::from([integer_value]),
                    },
                    &[canonical],
                    child_origin,
                )
                .expect("record")[0];
            function
                .terminate(
                    entry,
                    Terminator::new(TerminatorKind::Return(record_value), child_origin),
                )
                .expect("child return");
            (integer_value, boolean_value, record_value)
        };

        let root = builder
            .declare_function(
                root_origin,
                "alternative_task.root",
                Signature::new([], unit),
                Effects::NEEDS_EXECUTOR.with_implications(),
            )
            .expect("root");
        {
            let mut function = builder.function(root).expect("root builder");
            function
                .set_coroutine_plan(CoroutinePlan::new(unit, []))
                .expect("root coroutine");
            let entry = function.create_block().expect("root entry");
            function.set_entry(entry).expect("set root entry");
            function
                .append_instruction(
                    entry,
                    InstructionKind::TaskCreate {
                        coroutine: child,
                        arguments: Box::new([]),
                    },
                    &[task],
                    root_origin,
                )
                .expect("Task[record]");
            let result = function
                .append_instruction(
                    entry,
                    InstructionKind::Constant(Constant::Unit),
                    &[unit],
                    root_origin,
                )
                .expect("Unit")[0];
            function
                .terminate(
                    entry,
                    Terminator::new(TerminatorKind::Return(result), root_origin),
                )
                .expect("root return");
        }

        let mut program = builder.finish();
        validate_program(&program).expect("canonical Task ABI must validate");
        let alternative_product =
            ProductReprId::from_index(program.brand, program.representations.products.len())
                .expect("alternative product identity");
        let alternative_repr =
            ReprId::from_index(program.brand, program.representations.reprs.len())
                .expect("alternative representation identity");
        let alternative_type =
            ValueTypeId::from_index(program.brand, program.representations.types.len())
                .expect("alternative value type identity");
        program.representations.products.push(ProductRepr {
            fields: Box::from([integer, boolean]),
        });
        program
            .representations
            .reprs
            .push(Repr::Product(alternative_product));
        program.representations.types.push(ValueType {
            semantic,
            repr: alternative_repr,
            kind: ValueTypeKind::Direct,
        });

        let child_index = child.index();
        let child = &mut program.functions[child_index];
        child.signature = Signature::new([], alternative_type);
        child.coroutine = Some(CoroutinePlan::new(alternative_type, []));
        child.values[record_value.index()].ty = alternative_type;
        let ValueDefinition::InstructionResult { instruction, .. } =
            child.values[record_value.index()].definition
        else {
            panic!("record result must name its construction instruction")
        };
        child.instructions[instruction.index()].kind = InstructionKind::ProductConstruct {
            fields: Box::from([integer_value, boolean_value]),
        };

        let errors = validate_program(&program)
            .expect_err("Task handles must not erase a coroutine's exact result representation");
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::InvalidCoroutinePlan
                && error.path() == format!("function[{child_index}].coroutine.output")
                && error.message().contains("canonical value representation")
        }));
        assert!(errors.as_slice().iter().any(|error| {
            error.code() == ValidationCode::TypeMismatch
                && error.path().ends_with(".coroutine.result")
                && error.message().contains("canonical value representation")
        }));
    }

    #[test]
    fn semantic_alternatives_inherit_canonical_construction_protection() {
        let invariant_semantic = Type::Nominal(TypeId(73), Vec::new());
        let other_semantic = Type::Nominal(TypeId(74), Vec::new());
        let transparent_semantic = Type::Nominal(TypeId(75), Vec::new());
        let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let invariant = builder
            .add_invariant_record_type(invariant_semantic, &[Type::Int])
            .expect("canonical invariant product");
        let other = builder
            .add_transparent_type(other_semantic, &Type::Float)
            .expect("other transparent type");
        let transparent = builder
            .add_transparent_type(transparent_semantic, &Type::Float)
            .expect("canonical transparent type");
        let mut program = builder.finish();

        let mut unprotected_invariant = program.representations.types[invariant.index()].clone();
        unprotected_invariant.kind = ValueTypeKind::Direct;
        program.representations.types.push(unprotected_invariant);

        let mut unprotected_transparent =
            program.representations.types[transparent.index()].clone();
        unprotected_transparent.kind = ValueTypeKind::Direct;
        program.representations.types.push(unprotected_transparent);

        let mut wrong_base = program.representations.types[transparent.index()].clone();
        wrong_base.kind = ValueTypeKind::Transparent { base: other };
        program.representations.types.push(wrong_base);

        let errors = validate_program(&program)
            .expect_err("representation alternatives must not weaken canonical protection");
        assert_eq!(
            errors
                .as_slice()
                .iter()
                .filter(|error| error
                    .message()
                    .contains("canonical construction protection"))
                .count(),
            3
        );
    }

    #[test]
    fn transparent_base_must_be_physically_inhabited() {
        let semantic = Type::Nominal(TypeId(76), Vec::new());
        let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let float = builder.type_id(&Type::Float).expect("Float type");
        let transparent = builder
            .add_transparent_type(semantic, &Type::Float)
            .expect("transparent type");
        let mut program = builder.finish();
        let never_repr = program.representations.types[0].repr;
        program.representations.types[float.index()].repr = never_repr;
        program.representations.types[transparent.index()].repr = never_repr;

        let errors =
            validate_program(&program).expect_err("uninhabited transparent base must be rejected");
        assert!(errors.as_slice().iter().any(|error| {
            error.path() == format!("representations.type[{}].transparent", transparent.index())
                && error.message().contains("inhabited base")
        }));
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
