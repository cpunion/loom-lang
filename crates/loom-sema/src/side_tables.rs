//! Semantic facts keyed by immutable HIR identities.

use std::collections::BTreeMap;

use loom_hir::{
    ArenaMap, BodyId, DefId, ExprId, GenericParamId, LocalId, ParamId, PatternId, ReceiverKind,
    TypeRefId,
};
use serde::{Deserialize, Serialize};

use crate::{
    AssociatedTypeBinding, ConceptInstance, Mutability, Substitution, TyId, TyInterner,
    WitnessSelection,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Resolution {
    Definition(DefId),
    GenericParam(GenericParamId),
    Param(ParamId),
    Local(LocalId),
    SelfValue,
    ResultValue,
    Builtin(BuiltinValue),
    Error,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum BuiltinValue {
    Unit,
    None,
    Some,
    Ok,
    Err,
    ParseFloat,
    FormatFloat,
    IsFinite,
    TextLength,
    TextGet,
    TextConcat,
    TextContains,
    TextEncodeUtf8,
    BytesLength,
    BytesGet,
    BytesAppend,
    BytesDecodeUtf8,
    PathFromText,
    PathAsText,
    PathJoin,
    TextMapNew,
    TextMapLength,
    TextMapContains,
    TextMapGet,
    TextMapInsert,
    TextMapRemove,
    JsonNull,
    JsonBool,
    JsonNumber,
    JsonText,
    JsonArray,
    JsonObject,
    JsonParse,
    JsonFormat,
    JsonInvalidSyntax,
    JsonNumberOutOfRange,
    JsonDepthLimit,
    JsonNonFiniteNumber,
    IoErrorKind,
    IoErrorMessage,
    IoErrorNotFound,
    IoErrorPermissionDenied,
    IoErrorAlreadyExists,
    IoErrorInvalidInput,
    IoErrorConnectionRefused,
    IoErrorConnectionReset,
    IoErrorTimedOut,
    IoErrorUnexpectedEof,
    IoErrorClosed,
    IoErrorOther,
    LogLevelDebug,
    LogLevelInfo,
    LogLevelWarn,
    LogLevelError,
    LogDebug,
    LogInfo,
    LogWarn,
    LogError,
    LogWrite,
    ListNew,
    ListAdd,
    ListLength,
    ListGet,
    ProcessArguments,
    ProcessEnvironment,
    ParseInt,
    ParseFloatInvalidSyntax,
    ParseFloatOutOfRange,
    ParseIntInvalidSyntax,
    ParseIntOutOfRange,
    DecodeTextInvalidUtf8,
    PathContainsNul,
    PathAbsoluteJoin,
    TaskCompleted,
    TaskFaulted,
    TaskCancelled,
    TaskFaultCode,
    TaskFaultMessage,
    DurationMilliseconds,
    DurationAsMilliseconds,
    FileOpenRead,
    FileCreate,
    FileOpenReadPath,
    FileCreatePath,
    FileTryOpenRead,
    FileTryCreate,
    FileTryOpenReadPath,
    FileTryCreatePath,
    FileReadText,
    FileWriteText,
    FileTryReadText,
    FileTryWriteText,
    FileClose,
    SocketConnect,
    SocketTryConnect,
    SocketReadText,
    SocketWriteText,
    SocketTryReadText,
    SocketTryWriteText,
    SocketClose,
}

/// Stable semantic identity for a compiler-known standard-library API item.
/// Version 0.3 obtains this identity from the embedded standard-item catalog;
/// future source-library definitions can map their trusted definition identity
/// to the same value without changing MIR.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum StandardLibraryItem {
    TaskSleep,
    TaskAll,
    TaskSettled,
    TaskAny,
    TaskRace,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum CallTarget {
    Function(DefId),
    InherentMethod(DefId),
    EnumVariant(DefId),
    RefinedConstructor(DefId),
    Builtin(BuiltinValue),
    StandardLibrary(StandardLibraryItem),
    StaticConcept { requirement: DefId },
    DynamicConcept { requirement: DefId },
    Error,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ReceiverPassing {
    Value,
    InOut,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CallResolution {
    pub target: CallTarget,
    pub substitution: Substitution,
    /// The witness selecting a concept requirement implementation.  This is
    /// distinct from `witnesses`, which are hidden proof arguments required by
    /// the selected callable's own generic bounds.
    pub dispatch_witness: Option<WitnessSelection>,
    pub witnesses: Vec<WitnessSelection>,
    pub receiver: Option<ReceiverPassing>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct RegionId(pub u32);

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ViewTokenId(pub u32);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ViewResolution {
    pub source: ViewSource,
    pub mutable: bool,
    pub region: RegionId,
    pub token: ViewTokenId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ViewSource {
    Concrete {
        witness: WitnessSelection,
        writeback: Option<Place>,
    },
    Interface {
        owner: Place,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum PlaceRoot {
    Param(ParamId),
    Local(LocalId),
    SelfValue,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum PlaceProjection {
    Field(DefId),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Place {
    pub root: PlaceRoot,
    pub projections: Vec<PlaceProjection>,
    pub mutability: Mutability,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Coercion {
    RefinedToBase {
        refined: DefId,
    },
    /// A concrete value is exposed through a dynamically dispatched concept
    /// parameter. The owner and selected witness are stored in `views` under
    /// the same expression id; no source-level ownership syntax is involved.
    ConcreteToDyn,
    /// A first-class interface value is temporarily reborrowed when passed to
    /// another mutable interface parameter. Ordinary copies remain owned.
    InterfaceReborrow,
    NeverToAny,
}

/// Whether a constrained value or invariant-bearing record still needs its
/// runtime validation boundary.  `Proven` is emitted only by the closed,
/// deterministic proof engine; it changes the construction expression from a
/// `Result` into the established nominal value itself.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ConstructionCheck {
    Proven,
    Runtime,
}

/// Whether a pure, effect-restricted contract predicate must still execute at runtime.
/// A disproven predicate remains `Runtime`: unlike invalid checked
/// construction, an assertion or callable contract may intentionally expose a
/// faulting path and must retain its blame/reporting behavior.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum RuntimeCheck {
    Proven,
    Runtime,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct BodySemantics {
    pub expression_resolutions: ArenaMap<ExprId, Resolution>,
    pub expression_types: ArenaMap<ExprId, TyId>,
    pub expression_places: ArenaMap<ExprId, Place>,
    pub expression_coercions: ArenaMap<ExprId, Coercion>,
    pub calls: ArenaMap<ExprId, CallResolution>,
    /// Proof disposition for constrained constructors and invariant-bearing
    /// record literals. Expressions without a declared constraint/invariant
    /// are absent.
    pub construction_checks: ArenaMap<ExprId, ConstructionCheck>,
    /// Per-assert proof disposition, keyed by the assertion condition.
    pub assertion_checks: ArenaMap<ExprId, RuntimeCheck>,
    /// Proof disposition of this body when it is a refinement predicate,
    /// record invariant, requires, or ensures body.
    pub contract_check: Option<RuntimeCheck>,
    /// Source expressions mapped into definition field order. HIR retains the
    /// original source order so MIR can evaluate into temporaries first.
    pub record_fields: ArenaMap<ExprId, Vec<(DefId, ExprId)>>,
    pub views: ArenaMap<ExprId, ViewResolution>,
    /// Mutable-view path expressions which consume their source binding.  The
    /// tokens identify the possible borrow origins after control-flow joins;
    /// an expression absent from this table is a non-consuming receiver/read.
    pub view_moves: ArenaMap<ExprId, Vec<ViewTokenId>>,
    pub pattern_resolutions: ArenaMap<PatternId, Resolution>,
    pub pattern_types: ArenaMap<PatternId, TyId>,
    pub local_types: ArenaMap<LocalId, TyId>,
    /// Static Dispose dispatch selected for each `scoped` declaration.
    pub scoped_disposals: ArenaMap<LocalId, ScopedDisposal>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ScopedDisposal {
    Concept {
        requirement: DefId,
        witness: WitnessSelection,
    },
    Builtin(BuiltinValue),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CallableSignature {
    pub is_async: bool,
    /// All type parameters in scope for the callable body, including enclosing
    /// impl parameters.
    pub generic_params: Vec<GenericParamId>,
    /// Parameters introduced by this callable and therefore accepted by its
    /// source-level type-argument list.
    pub call_generic_params: Vec<GenericParamId>,
    pub receiver: Option<ReceiverKind>,
    pub params: Vec<(ParamId, TyId)>,
    pub return_ty: TyId,
    /// All proof parameters needed by the executable callable body.
    pub bounds: Vec<Bound>,
    /// Bounds introduced specifically by method/function type parameters.
    pub call_bounds: Vec<Bound>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Bound {
    pub self_ty: TyId,
    pub concept: ConceptInstance,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum Signature {
    Type { generic_params: Vec<GenericParamId> },
    Field { owner: DefId, ty: TyId },
    Variant { owner: DefId, payload: Vec<TyId> },
    Callable(CallableSignature),
    Concept,
    AssociatedType { owner: DefId, bounds: Vec<Bound> },
    Impl,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct TypedProgram {
    pub types: TyInterner,
    pub resolved_type_refs: ArenaMap<TypeRefId, TyId>,
    pub signatures: ArenaMap<DefId, Signature>,
    pub bodies: ArenaMap<BodyId, BodySemantics>,
    pub conformances: ArenaMap<DefId, ConformanceSemantics>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ConformanceSemantics {
    pub concept: ConceptInstance,
    pub target: TyId,
    pub methods: BTreeMap<DefId, DefId>,
    pub associated_types: Vec<AssociatedTypeBinding>,
}

impl TypedProgram {
    #[must_use]
    pub fn body(&self, body: BodyId) -> Option<&BodySemantics> {
        self.bodies.get(body)
    }
}
