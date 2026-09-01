use std::fmt;

use loom_core::Span;
use loom_mir::{ExprId as MirExprId, FunctionId as MirFunctionId, PreludeIds, TypeId};

use crate::ids::ProgramBrand;
use crate::{
    BlockId, InstanceId, InstanceKey, InstancePlan, InstructionId, RepresentationPlan, ValueId,
    ValueTypeId,
};

/// A target-specific LCIR program before or after independent validation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Program {
    pub(crate) brand: ProgramBrand,
    pub(crate) canonical_types: Box<CanonicalTypeCatalog>,
    pub(crate) representations: RepresentationPlan,
    pub(crate) instances: InstancePlan,
    pub(crate) functions: Vec<Function>,
}

impl Program {
    /// Returns the checked-MIR identities which give compiler-known standard
    /// types their meaning in this LCIR program.
    #[must_use]
    pub const fn canonical_types(&self) -> &CanonicalTypeCatalog {
        &self.canonical_types
    }

    #[must_use]
    pub const fn representations(&self) -> &RepresentationPlan {
        &self.representations
    }

    #[must_use]
    pub const fn instances(&self) -> &InstancePlan {
        &self.instances
    }

    #[must_use]
    pub fn instance_key(&self, id: InstanceId) -> Option<&InstanceKey> {
        self.instances.key(id)
    }

    #[must_use]
    pub fn functions(&self) -> &[Function] {
        &self.functions
    }

    #[must_use]
    pub fn function(&self, id: InstanceId) -> Option<&Function> {
        (self.instances.key(id).is_some())
            .then(|| self.functions.get(id.index()))
            .flatten()
            .filter(|function| function.id == id)
    }
}

/// Source-definition identities used by typed standard-library lowering.
///
/// LCIR deliberately carries these identities instead of assigning standard
/// types fixed numeric slots. Every entry is optional because focused LCIR
/// programs may omit facilities they do not use. Instructions and
/// representations which do use a facility independently require its exact
/// catalog entry during validation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CanonicalTypeCatalog {
    pub result: Option<TypeId>,
    pub option: Option<TypeId>,
    pub constraint_error: Option<TypeId>,
    pub task_fault: Option<TypeId>,
    pub task_outcome: Option<TypeId>,
    pub duration: Option<TypeId>,
    pub file: Option<TypeId>,
    pub socket: Option<TypeId>,
    pub bytes: Option<TypeId>,
    pub path: Option<TypeId>,
    pub decode_text_error: Option<TypeId>,
    pub path_error: Option<TypeId>,
    pub text_map: Option<TypeId>,
    pub json: Option<TypeId>,
    pub json_error: Option<TypeId>,
    pub io_error: Option<TypeId>,
    pub io_error_kind: Option<TypeId>,
    pub log_level: Option<TypeId>,
}

impl CanonicalTypeCatalog {
    /// Copies only type identities from checked MIR's wider prelude catalog.
    #[must_use]
    pub const fn from_prelude(prelude: &PreludeIds) -> Self {
        Self {
            result: prelude.result,
            option: prelude.option,
            constraint_error: prelude.constraint_error,
            task_fault: prelude.task_fault,
            task_outcome: prelude.task_outcome,
            duration: prelude.duration,
            file: prelude.file,
            socket: prelude.socket,
            bytes: prelude.bytes,
            path: prelude.path,
            decode_text_error: prelude.decode_text_error,
            path_error: prelude.path_error,
            text_map: prelude.text_map,
            json: prelude.json,
            json_error: prelude.json_error,
            io_error: prelude.io_error,
            io_error_kind: prelude.io_error_kind,
            log_level: prelude.log_level,
        }
    }
}

/// Transitive runtime behavior represented by a lowered function.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Effects(u8);

impl Effects {
    const MAY_FAULT_BIT: u8 = 1;
    const NEEDS_RUNTIME_BIT: u8 = 1 << 1;
    const MAY_COLLECT_BIT: u8 = 1 << 2;
    const NEEDS_EXECUTOR_BIT: u8 = 1 << 3;
    const MAY_SUSPEND_BIT: u8 = 1 << 4;

    pub const NONE: Self = Self(0);
    pub const MAY_FAULT: Self = Self(Self::MAY_FAULT_BIT);
    pub const NEEDS_RUNTIME: Self = Self(Self::NEEDS_RUNTIME_BIT);
    pub const MAY_COLLECT: Self = Self(Self::MAY_COLLECT_BIT);
    pub const NEEDS_EXECUTOR: Self = Self(Self::NEEDS_EXECUTOR_BIT);
    pub const MAY_SUSPEND: Self = Self(Self::MAY_SUSPEND_BIT);

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    #[must_use]
    pub const fn contains(self, required: Self) -> bool {
        self.0 & required.0 == required.0
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Returns the least effect set which includes every capability implied by
    /// this set. Collection requires an active runtime, and suspension requires
    /// an executor which itself requires an active runtime. Source faults are
    /// deliberately independent: checked scalar faults need a fault context,
    /// but do not create a Loom runtime or executor.
    #[must_use]
    pub const fn with_implications(self) -> Self {
        let mut bits = self.0;
        if bits & Self::MAY_COLLECT_BIT != 0 {
            bits |= Self::NEEDS_RUNTIME_BIT;
        }
        if bits & Self::MAY_SUSPEND_BIT != 0 {
            bits |= Self::NEEDS_EXECUTOR_BIT;
        }
        if bits & Self::NEEDS_EXECUTOR_BIT != 0 {
            bits |= Self::NEEDS_RUNTIME_BIT;
        }
        Self(bits)
    }

    #[must_use]
    pub const fn is_closed(self) -> bool {
        self.0 == self.with_implications().0
    }

    #[must_use]
    pub(crate) const fn without(self, removed: Self) -> Self {
        Self(self.0 & !removed.0)
    }
}

impl fmt::Display for Effects {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return formatter.write_str("none");
        }
        let mut separator = "";
        for (effect, name) in [
            (Self::MAY_FAULT, "may_fault"),
            (Self::NEEDS_RUNTIME, "needs_runtime"),
            (Self::MAY_COLLECT, "may_collect"),
            (Self::NEEDS_EXECUTOR, "needs_executor"),
            (Self::MAY_SUSPEND, "may_suspend"),
        ] {
            if self.contains(effect) {
                formatter.write_str(separator)?;
                formatter.write_str(name)?;
                separator = "+";
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Signature {
    params: Box<[ValueTypeId]>,
    result: ValueTypeId,
    /// Parameter positions whose current values are returned as ordered
    /// functional writebacks on both normal and fault exits.
    inout_params: Box<[u32]>,
}

impl Signature {
    #[must_use]
    pub fn new(params: impl Into<Box<[ValueTypeId]>>, result: ValueTypeId) -> Self {
        Self {
            params: params.into(),
            result,
            inout_params: Box::new([]),
        }
    }

    /// Constructs a signature with explicit functional inout parameters.
    /// Independent validation requires the positions to be strictly ordered,
    /// in range, and backed by canonical direct Bool, Int, or Float, a direct
    /// product, or a closed dynamic value.
    #[must_use]
    pub fn with_inout_params(
        params: impl Into<Box<[ValueTypeId]>>,
        result: ValueTypeId,
        inout_params: impl Into<Box<[u32]>>,
    ) -> Self {
        Self {
            params: params.into(),
            result,
            inout_params: inout_params.into(),
        }
    }

    #[must_use]
    pub const fn params(&self) -> &[ValueTypeId] {
        &self.params
    }

    #[must_use]
    pub const fn result(&self) -> ValueTypeId {
        self.result
    }

    #[must_use]
    pub const fn inout_params(&self) -> &[u32] {
        &self.inout_params
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Function {
    pub(crate) id: InstanceId,
    pub(crate) origin: Origin,
    pub(crate) name: String,
    pub(crate) signature: Signature,
    pub(crate) effects: Effects,
    pub(crate) coroutine: Option<CoroutinePlan>,
    pub(crate) entry: Option<BlockId>,
    pub(crate) blocks: Vec<Block>,
    pub(crate) instructions: Vec<Instruction>,
    pub(crate) values: Vec<Value>,
}

impl Function {
    #[must_use]
    pub const fn id(&self) -> InstanceId {
        self.id
    }

    #[must_use]
    pub const fn source(&self) -> MirFunctionId {
        self.origin.source_function
    }

    #[must_use]
    pub const fn origin(&self) -> Origin {
        self.origin
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn signature(&self) -> &Signature {
        &self.signature
    }

    #[must_use]
    pub const fn effects(&self) -> Effects {
        self.effects
    }

    /// Returns the checked stackless-coroutine frame contract for an async
    /// function. Synchronous functions have no coroutine plan.
    #[must_use]
    pub const fn coroutine(&self) -> Option<&CoroutinePlan> {
        self.coroutine.as_ref()
    }

    #[must_use]
    pub const fn entry(&self) -> Option<BlockId> {
        self.entry
    }

    #[must_use]
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    #[must_use]
    pub fn instructions(&self) -> &[Instruction] {
        &self.instructions
    }

    #[must_use]
    pub fn values(&self) -> &[Value] {
        &self.values
    }

    #[must_use]
    pub fn block(&self, id: BlockId) -> Option<&Block> {
        (id.owner() == self.id)
            .then(|| self.blocks.get(id.index()))
            .flatten()
    }

    #[must_use]
    pub fn instruction(&self, id: InstructionId) -> Option<&Instruction> {
        (id.owner() == self.id)
            .then(|| self.instructions.get(id.index()))
            .flatten()
    }

    #[must_use]
    pub fn value(&self, id: ValueId) -> Option<&Value> {
        (id.owner() == self.id)
            .then(|| self.values.get(id.index()))
            .flatten()
    }
}

/// Target-typed logical frame contract for one stackless coroutine instance.
///
/// Parameter and result slots are implied by the function signature. Each
/// suspension row lists the exact child output types which determine either
/// the completed values (`all`/`any`) or terminal child handles
/// (`settled`/`race`) injected before, in deterministic local-id order, the
/// exact live values forwarded to its continuation. The LLVM backend derives
/// byte offsets and precise managed-pointer projections from these checked
/// value types; no universal value envelope or interpreter frame is involved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoroutinePlan {
    output: ValueTypeId,
    suspensions: Box<[CoroutineSuspension]>,
    carries_caller_span: bool,
}

impl CoroutinePlan {
    #[must_use]
    pub fn new(output: ValueTypeId, suspensions: impl Into<Box<[CoroutineSuspension]>>) -> Self {
        Self {
            output,
            suspensions: suspensions.into(),
            carries_caller_span: false,
        }
    }

    /// Adds the compiler-private source span of the `TaskCreate` call to the
    /// coroutine frame. A checked plan may carry it only when a precondition
    /// fault in this function uses [`ContractFaultBlame::CoroutineCallSite`].
    #[must_use]
    pub const fn with_caller_span(mut self) -> Self {
        self.carries_caller_span = true;
        self
    }

    #[must_use]
    pub const fn output(&self) -> ValueTypeId {
        self.output
    }

    #[must_use]
    pub const fn suspensions(&self) -> &[CoroutineSuspension] {
        &self.suspensions
    }

    #[must_use]
    pub const fn carries_caller_span(&self) -> bool {
        self.carries_caller_span
    }
}

/// One nonzero MIR resume state, its exact child Task output types, join mode,
/// and forwarded live-value types. `awaited` always describes the child output
/// `T`, including modes whose normal edge injects a terminal `Task[T]` handle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoroutineSuspension {
    state: u32,
    mode: AwaitMode,
    awaited: Box<[ValueTypeId]>,
    live: Box<[ValueTypeId]>,
}

impl CoroutineSuspension {
    #[must_use]
    pub fn new(
        state: u32,
        mode: AwaitMode,
        awaited: impl Into<Box<[ValueTypeId]>>,
        live: impl Into<Box<[ValueTypeId]>>,
    ) -> Self {
        Self {
            state,
            mode,
            awaited: awaited.into(),
            live: live.into(),
        }
    }

    #[must_use]
    pub const fn state(&self) -> u32 {
        self.state
    }

    #[must_use]
    pub const fn mode(&self) -> AwaitMode {
        self.mode
    }

    #[must_use]
    pub const fn awaited(&self) -> &[ValueTypeId] {
        &self.awaited
    }

    #[must_use]
    pub const fn live(&self) -> &[ValueTypeId] {
        &self.live
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Block {
    pub(crate) id: BlockId,
    pub(crate) params: Vec<ValueId>,
    pub(crate) instructions: Vec<InstructionId>,
    pub(crate) terminator: Option<Terminator>,
}

impl Block {
    #[must_use]
    pub const fn id(&self) -> BlockId {
        self.id
    }

    #[must_use]
    pub fn params(&self) -> &[ValueId] {
        &self.params
    }

    #[must_use]
    pub fn instructions(&self) -> &[InstructionId] {
        &self.instructions
    }

    #[must_use]
    pub const fn terminator(&self) -> Option<&Terminator> {
        self.terminator.as_ref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Value {
    pub(crate) id: ValueId,
    pub(crate) ty: ValueTypeId,
    pub(crate) definition: ValueDefinition,
}

impl Value {
    #[must_use]
    pub const fn id(&self) -> ValueId {
        self.id
    }

    #[must_use]
    pub const fn ty(&self) -> ValueTypeId {
        self.ty
    }

    #[must_use]
    pub const fn definition(&self) -> ValueDefinition {
        self.definition
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValueDefinition {
    BlockParameter {
        block: BlockId,
        index: u32,
    },
    InstructionResult {
        instruction: InstructionId,
        index: u32,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Instruction {
    pub(crate) id: InstructionId,
    pub(crate) results: Vec<ValueId>,
    pub(crate) kind: InstructionKind,
    pub(crate) origin: Origin,
}

impl Instruction {
    #[must_use]
    pub const fn id(&self) -> InstructionId {
        self.id
    }

    #[must_use]
    pub fn results(&self) -> &[ValueId] {
        &self.results
    }

    #[must_use]
    pub const fn kind(&self) -> &InstructionKind {
        &self.kind
    }

    #[must_use]
    pub const fn origin(&self) -> Origin {
        self.origin
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Origin {
    pub source_function: MirFunctionId,
    pub expression: Option<MirExprId>,
    pub span: Span,
}

impl Origin {
    #[must_use]
    pub const fn synthetic(source_function: MirFunctionId) -> Self {
        Self {
            source_function,
            expression: None,
            span: Span {
                file: loom_core::FileId(0),
                range: loom_core::TextRange { start: 0, end: 0 },
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Constant {
    Unit,
    Bool(bool),
    Int(i64),
    FloatBits(u64),
}

impl Constant {
    #[must_use]
    pub fn float(value: f64) -> Self {
        Self::FloatBits(value.to_bits())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FloatBinaryOp {
    Add,
    Subtract,
    Multiply,
    Divide,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BoolPredicate {
    Equal,
    NotEqual,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckedIntBinaryOp {
    /// Faults with [`FaultCode::IntegerOverflow`] on signed overflow.
    Add,
    /// Faults with [`FaultCode::IntegerOverflow`] on signed overflow.
    Subtract,
    /// Faults with [`FaultCode::IntegerOverflow`] on signed overflow.
    Multiply,
    /// Distinguishes [`FaultCode::IntegerDivisionByZero`] from
    /// [`FaultCode::IntegerDivisionOverflow`] in the active fault context.
    Divide,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntPredicate {
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FloatPredicate {
    OrderedEqual,
    UnorderedNotEqual,
    OrderedLess,
    OrderedLessEqual,
    OrderedGreater,
    OrderedGreaterEqual,
}

/// Maximum operands copied into one typed List-construction instruction.
/// Runtime byte limits are validated independently from this hostile-artifact
/// structural bound.
/// Maximum elements carried by one checked LCIR list-construction instruction.
///
/// The emitter allocates the backing once and streams stores iteratively, so
/// this is an artifact-size guard rather than a stack or runtime-growth limit.
pub const LIST_LITERAL_MAX_ELEMENTS: usize = 65_536;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstructionKind {
    Constant(Constant),
    /// Constructs one process-lifetime UTF-8 Text object. Its result uses the
    /// artifact's canonical Text pointer representation: `ImmortalText` for a
    /// literal-only artifact or `ManagedPointer` when concat is reachable. No
    /// instruction may construct either representation from raw bits or a
    /// foreign pointer.
    TextLiteral {
        utf8: Box<str>,
    },
    /// Concatenates two complete Text objects. This is an infallible language
    /// operation with a moving-GC safepoint; resource exhaustion is a terminal
    /// runtime fault rather than an LCIR unwind edge.
    TextConcat {
        left: ValueId,
        right: ValueId,
    },
    /// Selects one Unicode scalar by signed scalar index. The result is the
    /// canonical closed `Option[Text]` sum: `missing_variant` has no payload
    /// and `found_variant` carries one managed Text. Missing selection does
    /// not allocate; successful selection is a moving-GC safepoint.
    TextGet {
        text: ValueId,
        index: ValueId,
        missing_variant: u32,
        found_variant: u32,
    },
    /// Reads the cached Unicode scalar length from a canonical Text object,
    /// whether its checked representation is immortal or managed.
    TextLength {
        text: ValueId,
    },
    /// Tests UTF-8 byte-subsequence containment without allocating or reaching
    /// a moving-GC safepoint.
    TextContains {
        text: ValueId,
        needle: ValueId,
    },
    /// Compares Text contents. Pointer identity is never language equality.
    TextCompare {
        predicate: BoolPredicate,
        left: ValueId,
        right: ValueId,
    },
    /// Reinterprets one canonical Text object as its exact immutable UTF-8
    /// byte sequence. Text and Bytes share the managed object prefix, so this
    /// operation preserves the object pointer and cannot allocate.
    TextEncodeUtf8 {
        text: ValueId,
    },
    /// Reads the immutable process argument snapshot length captured by the
    /// generated entry point. The executable name is not part of the snapshot.
    ProcessArgumentCount,
    /// Selects one argument from the immutable process snapshot and publishes
    /// it as a freshly allocated canonical managed Text. Source std.process
    /// calls this only with an index proved by its `0..count` loop; an invalid
    /// index is therefore a compiler/runtime ABI defect rather than a language
    /// Option result.
    ProcessArgumentAt {
        index: ValueId,
    },
    /// Looks up one Unicode process-environment value. The result is the exact
    /// canonical `Option[Text]`; a missing or non-Unicode host value selects
    /// `missing_variant`, while a present value is copied into managed Text and
    /// selects `found_variant`.
    ProcessEnvironment {
        name: ValueId,
        missing_variant: u32,
        found_variant: u32,
    },
    /// Validates one canonical managed Text as a lexical path and constructs
    /// exact `Result[Path, PathError]`. A NUL byte selects the closed
    /// `ContainsNul` error; success wraps the existing Text pointer without
    /// allocation or a moving-GC safepoint.
    PathFromText {
        text: ValueId,
        ok_variant: u32,
        error_variant: u32,
        contains_nul_variant: u32,
    },
    /// Extracts the immutable managed Text field from cataloged canonical `Path`.
    /// This is a representation-preserving, non-allocating operation.
    PathAsText {
        path: ValueId,
    },
    /// Lexically joins two canonical Path values and constructs exact
    /// `Result[Path, PathError]`. An absolute child selects `AbsoluteJoin`;
    /// success stages the complete UTF-8 bytes before allocating at most one
    /// managed Text object, making this instruction a moving-GC safepoint.
    PathJoin {
        base: ValueId,
        child: ValueId,
        ok_variant: u32,
        error_variant: u32,
        absolute_join_variant: u32,
    },
    /// Materializes one source-level copy of a canonical List or Bytes value.
    /// The result is representation-identical to `value`; the instruction has
    /// no runtime operation, but makes the COW alias boundary explicit so
    /// checked ownership analysis marks both SSA values shared.
    CollectionShare {
        value: ValueId,
    },
    /// Reads the exact byte length from a canonical managed Bytes object.
    BytesLength {
        bytes: ValueId,
    },
    /// Performs a checked signed byte index and constructs canonical
    /// `Option[Int]`: `missing_variant` carries no payload and
    /// `found_variant` carries the selected unsigned byte widened to Int.
    BytesGet {
        bytes: ValueId,
        index: ValueId,
        missing_variant: u32,
        found_variant: u32,
    },
    /// Concatenates two immutable byte sequences into one freshly allocated
    /// canonical managed Bytes object.
    BytesAppend {
        left: ValueId,
        right: ValueId,
    },
    /// Produces a new canonical Bytes value containing all old units followed
    /// by `unit`. `lower_proof` and `upper_proof` must be the exact checked
    /// results of `unit >= 0` and `unit <= 255`; their successful guard edges
    /// must dominate this instruction. The receiver remains independently
    /// usable. A backend may allocate exact typed byte storage at this
    /// moving-GC safepoint.
    BytesPush {
        bytes: ValueId,
        unit: ValueId,
        lower_proof: ValueId,
        upper_proof: ValueId,
    },
    /// Pushes through a checked-MIR eligibility certificate and the same exact
    /// byte-range proof contract as [`InstructionKind::BytesPush`]. The
    /// receiver is consumed and the result carries the certificate forward.
    /// Backends must still inspect the managed-object descriptor before
    /// reusing storage: Text-backed UTF-8 views are never mutable, even when
    /// this certificate proves that the Bytes SSA value itself has not been
    /// shared.
    BytesPushUnique {
        bytes: ValueId,
        unit: ValueId,
        lower_proof: ValueId,
        upper_proof: ValueId,
    },
    /// Validates canonical Bytes as UTF-8 and constructs exact
    /// `Result[Text, DecodeTextError]`. Successful decoding relabels canonical
    /// byte storage as immutable Text without moving or allocating it; invalid
    /// UTF-8 selects the nested error variant.
    BytesDecodeUtf8 {
        bytes: ValueId,
        ok_variant: u32,
        error_variant: u32,
        invalid_utf8_variant: u32,
    },
    /// Compares immutable byte contents. Managed pointer identity is never
    /// language equality.
    BytesCompare {
        predicate: BoolPredicate,
        left: ValueId,
        right: ValueId,
    },
    /// Parses one canonical Text value as a binary64 `Float` and returns
    /// `(value, status)`. Status 0 is success, 1 is invalid syntax, and 2 is
    /// out of range. Failed parses return zero in the first field; that field
    /// is compiler-private and unobservable.
    FloatParseStatus {
        text: ValueId,
    },
    /// Formats one binary64 `Float` into a freshly allocated canonical Text.
    /// The typed runtime publishes the managed pointer through a stable output
    /// cell at this instruction's exact collection safepoint.
    FloatFormat {
        value: ValueId,
    },
    /// Converts a signed 64-bit Int to IEEE-754 binary64 using
    /// round-to-nearest, ties-to-even.
    IntToFloat {
        value: ValueId,
    },
    /// Converts a binary64 Float toward zero when it is representable and
    /// returns `(converted, status)`. Status 0 is success, 1 is non-finite,
    /// and 2 is outside the signed 64-bit Int range. Failed conversions return
    /// zero in the first field; that field is compiler-private and unobservable.
    FloatToIntStatus {
        value: ValueId,
    },
    /// Constructs an immutable product value from fields in representation
    /// order. The result's checked value type selects the product definition.
    ProductConstruct {
        fields: Box<[ValueId]>,
    },
    /// Constructs an immutable product whose source invariant was discharged
    /// by semantic analysis. Independent validation requires the result type
    /// to be registered as an invariant product; ordinary product
    /// construction cannot create that semantic type.
    InvariantRecordProven {
        fields: Box<[ValueId]>,
    },
    /// Reads one field from an immutable product value.
    ProductExtract {
        aggregate: ValueId,
        field: u32,
    },
    /// Borrows one complete Task-bearing carrier without transferring its
    /// affine ownership. The result has the exact operand type and may only
    /// feed structural borrow operations or borrowed CFG forwarding.
    TaskCarrierBorrow {
        value: ValueId,
    },
    /// Borrows one field from an immutable Task-bearing product without
    /// transferring the aggregate's affine ownership. A Task-bearing result
    /// is a non-consuming alias which may only feed other structural borrow
    /// operations; independent validation rejects every consuming use.
    ProductBorrow {
        aggregate: ValueId,
        field: u32,
    },
    /// Reads one exact Task-free leaf through a Task-bearing product without
    /// transferring ownership of the aggregate or materializing affine
    /// intermediate fields. `path` is nonempty and follows product fields from
    /// the aggregate type to the result type.
    TaskCarrierProject {
        aggregate: ValueId,
        path: Box<[u32]>,
    },
    /// Atomically consumes one ordinary structural tuple and produces all of
    /// its fields in source order. Unlike [`Self::ProductExtract`] and
    /// [`Self::ProductBorrow`], this is a consuming decomposition boundary for
    /// affine Task-bearing aggregates; the aggregate cannot be used again
    /// after the split.
    ProductSplit {
        aggregate: ValueId,
    },
    /// Produces a new product value by replacing one field. The input remains
    /// independently usable, preserving source copy semantics in SSA.
    ProductInsert {
        aggregate: ValueId,
        field: u32,
        value: ValueId,
    },
    /// Atomically consumes one Task-bearing product and replaces one exact
    /// Task-free leaf. Affine siblings move into the result exactly once;
    /// intermediate Task-bearing products never become independent values.
    TaskCarrierUpdate {
        aggregate: ValueId,
        path: Box<[u32]>,
        value: ValueId,
    },
    /// Rebuilds a mutable receiver whose declared record invariant is checked
    /// at the function boundary. This compiler-proof operation is not source
    /// constructible: the intermediate SSA value is not an independently
    /// established invariant proof and may leave the function only after the
    /// exit invariant passes.
    InvariantReceiverInsert {
        aggregate: ValueId,
        field: u32,
        value: ValueId,
    },
    /// Establishes a transparent refined nominal type from its exact declared
    /// base after a compiler proof. The source and result have the same
    /// physical representation but distinct semantic value-type identities.
    RefineProven {
        value: ValueId,
    },
    /// Explicitly observes a transparent refined value as its exact declared
    /// base. This is representation-preserving and cannot target an arbitrary
    /// type with the same physical layout.
    Unrefine {
        value: ValueId,
    },
    /// Borrows the exact base representation of a transparent Task-bearing
    /// refined value. The result retains no ownership right and cannot cross
    /// a consuming LCIR boundary.
    UnrefineBorrow {
        value: ValueId,
    },
    /// Constructs one closed sum variant. The result type selects the sum
    /// representation and `variant` selects its ordered payload signature.
    SumConstruct {
        variant: u32,
        payload: Box<[ValueId]>,
    },
    /// Allocates one candidate payload in the result View's closed managed
    /// dynamic representation. `variant` is the compiler-private candidate
    /// ordinal, not a source-observable runtime type identity.
    DynConstruct {
        variant: u32,
        value: ValueId,
    },
    /// Constructs one immutable concrete List. Empty construction yields the
    /// canonical null value and does not collect; nonempty construction is a
    /// typed repeated-allocation safepoint.
    ListConstruct {
        elements: Box<[ValueId]>,
    },
    /// Produces a new immutable List containing all old elements followed by
    /// `value`. Both operands remain independently usable and must be rooted
    /// across the typed repeated-allocation safepoint.
    ListAppend {
        list: ValueId,
        value: ValueId,
    },
    /// Appends through a checked-MIR uniqueness certificate. The receiver is
    /// consumed and the result becomes its sole owner; independent validation
    /// proves the certificate across CFG edges before a backend may reuse the
    /// backing allocation.
    ListAppendUnique {
        list: ValueId,
        value: ValueId,
    },
    /// Returns zero for the canonical null empty List, otherwise its checked
    /// nonnegative element count. This operation cannot collect.
    ListLength {
        list: ValueId,
    },
    /// Performs a checked read and returns the canonical Option[element]
    /// closed sum. Null, negative, and out-of-bounds indexes produce None.
    ListGet {
        list: ValueId,
        index: ValueId,
    },
    /// Constructs the canonical empty compiler-private `TextMap[V]`. The
    /// representation is the null managed pointer, so this operation does not
    /// allocate or collect.
    TextMapConstruct,
    /// Produces a new immutable map with `key` bound to `value`. The old map
    /// remains independently usable; the backend allocates exact typed
    /// repeated storage and copies/replaces entries at a moving-GC safepoint.
    TextMapInsert {
        map: ValueId,
        key: ValueId,
        value: ValueId,
    },
    /// Builds one canonical sorted `TextMap` from a `List[(Text, V)]` in one bulk
    /// allocation. Duplicate input keys return the lexicographically smallest
    /// duplicate `Text` through the exact `Result[TextMap[V], Text]` result.
    TextMapConstructEntries {
        entries: ValueId,
    },
    /// Returns zero for the canonical empty map and otherwise its exact entry
    /// count. This operation cannot collect.
    TextMapLength {
        map: ValueId,
    },
    /// Tests whether the canonical sorted map contains `key` without loading
    /// or erasing its exact value type. This operation cannot allocate.
    TextMapContains {
        map: ValueId,
        key: ValueId,
    },
    /// Looks up a Text key without allocation and returns the canonical exact
    /// `Option[V]` selected by the result type.
    TextMapGet {
        map: ValueId,
        key: ValueId,
    },
    /// Produces a new immutable map without `key`. A missing key returns the
    /// original logical value; an existing key allocates and copies exact
    /// typed repeated storage without mutating aliases.
    TextMapRemove {
        map: ValueId,
        key: ValueId,
    },
    /// Reads one canonical sorted entry for compiler-generated structural
    /// equality. The exact result is `Option[(Text, V)]`; negative and
    /// out-of-bounds indexes produce None without allocation.
    TextMapEntryGet {
        map: ValueId,
        index: ValueId,
    },
    /// Allocates, initializes, and publishes one structured typed Task for a
    /// checked coroutine instance. The current checked executor may come
    /// directly from a coroutine callback or through one or more synchronous
    /// helper calls; helpers borrow it and never create or drive an executor.
    TaskCreate {
        coroutine: InstanceId,
        arguments: Box<[ValueId]>,
    },
    /// Creates one runtime-owned typed I/O leaf Task. The operation and error
    /// mode fix the exact argument and either `Task[Result[T, IoError]]` or
    /// faulting `Task[T]` shape; the runtime copies every borrowed source value
    /// before this instruction returns. Completion crosses the private ABI only
    /// as primitive status data, and a compiler-generated callback constructs
    /// the target-native direct result or records the operation-specific fault
    /// without a universal value envelope.
    IoTaskCreate {
        operation: IoTaskOperation,
        error_mode: IoTaskErrorMode,
        arguments: Box<[ValueId]>,
    },
    /// Closes one compiler-known File or Socket token without a universal
    /// value envelope. Final RAII release produces Unit and functionally
    /// returns the closed resource as the second result. Runtime status zero
    /// is the only ordinary outcome; every nonzero status is a defect.
    ResourceClose {
        kind: ResourceKind,
        resource: ValueId,
    },
    /// Allocates one exact fixed-width composite Task. The child tasks are
    /// consumed in source order and `mode` fixes the canonical result shape:
    /// `all` and `settled` preserve the complete heterogeneous row, while
    /// `any` and `race` require one homogeneous child output type.
    TaskJoin {
        mode: AwaitMode,
        tasks: Box<[ValueId]>,
    },
    /// Allocates one runtime-width composite Task from an exact
    /// `List[Task[T]]` carrier. The List and every child handle it owns are
    /// consumed together. Unlike [`Self::TaskJoin`], every mode has one
    /// homogeneous child output and preserves the source-visible List shape:
    /// `all` produces `Task[List[T]]`, `any` produces `Task[T]`, `settled`
    /// produces `Task[List[TaskOutcome[T]]]`, and `race` produces
    /// `Task[TaskOutcome[T]]`.
    TaskJoinList {
        mode: AwaitMode,
        tasks: ValueId,
    },
    /// Consumes one terminal child handle produced by a `settled` or `race`
    /// await and constructs its exact canonical `TaskOutcome[T]`. Completed
    /// payloads are moved from the child frame, fault code/message bytes become
    /// freshly allocated canonical Text objects, and cancelled outcomes carry
    /// no payload. The runtime helper publishes those exact payload components
    /// at this instruction's explicit moving-GC safepoint before retiring the
    /// child; the backend constructs the validated closed sum.
    TaskOutcomeTake {
        task: ValueId,
    },
    BoolNot {
        value: ValueId,
    },
    BoolCompare {
        predicate: BoolPredicate,
        left: ValueId,
        right: ValueId,
    },
    FloatNegate {
        value: ValueId,
    },
    FloatBinary {
        op: FloatBinaryOp,
        left: ValueId,
        right: ValueId,
    },
    IntCompare {
        predicate: IntPredicate,
        left: ValueId,
        right: ValueId,
    },
    /// Computes the signed mathematical successor of `value` without a
    /// runtime overflow edge.
    ///
    /// Checked LCIR validation requires `proof` to be the exact result of
    /// `value < upper_bound` and requires the comparison's true edge to
    /// dominate this instruction. Since `upper_bound` is an `Int`, that fact
    /// proves `value + 1` is representable. Backends may therefore emit a
    /// signed no-overflow add.
    IntSuccessorBelow {
        value: ValueId,
        upper_bound: ValueId,
        proof: ValueId,
    },
    FloatCompare {
        predicate: FloatPredicate,
        left: ValueId,
        right: ValueId,
    },
    /// Direct calls are deliberately limited to exactly infallible callees.
    /// A call which can fault is represented by [`TerminatorKind::Invoke`],
    /// so its result cannot exist on the unwind edge.
    DirectCall {
        callee: InstanceId,
        arguments: Box<[ValueId]>,
    },
}

impl InstructionKind {
    pub(crate) fn operands(&self) -> Vec<ValueId> {
        match self {
            Self::Constant(_)
            | Self::TextLiteral { .. }
            | Self::TextMapConstruct
            | Self::ProcessArgumentCount => Vec::new(),
            Self::TextLength { text: value }
            | Self::TextEncodeUtf8 { text: value }
            | Self::PathFromText { text: value, .. }
            | Self::FloatParseStatus { text: value }
            | Self::CollectionShare { value }
            | Self::ProcessArgumentAt { index: value }
            | Self::ProcessEnvironment { name: value, .. }
            | Self::PathAsText { path: value }
            | Self::BytesLength { bytes: value }
            | Self::BytesDecodeUtf8 { bytes: value, .. }
            | Self::TaskCarrierBorrow { value }
            | Self::RefineProven { value }
            | Self::Unrefine { value }
            | Self::UnrefineBorrow { value }
            | Self::BoolNot { value }
            | Self::FloatNegate { value }
            | Self::FloatFormat { value }
            | Self::IntToFloat { value }
            | Self::FloatToIntStatus { value }
            | Self::DynConstruct { value, .. }
            | Self::ResourceClose {
                resource: value, ..
            } => vec![*value],
            Self::TextGet { text, index, .. } => vec![*text, *index],
            Self::BytesGet { bytes, index, .. } => vec![*bytes, *index],
            Self::BytesPush {
                bytes,
                unit,
                lower_proof,
                upper_proof,
            }
            | Self::BytesPushUnique {
                bytes,
                unit,
                lower_proof,
                upper_proof,
            } => vec![*bytes, *unit, *lower_proof, *upper_proof],
            Self::ListConstruct { elements } => elements.to_vec(),
            Self::ListLength { list } => vec![*list],
            Self::TextContains { text, needle } => vec![*text, *needle],
            Self::ProductConstruct { fields } | Self::InvariantRecordProven { fields } => {
                fields.to_vec()
            }
            Self::ProductExtract { aggregate, .. }
            | Self::ProductBorrow { aggregate, .. }
            | Self::TaskCarrierProject { aggregate, .. }
            | Self::ProductSplit { aggregate } => vec![*aggregate],
            Self::ProductInsert {
                aggregate, value, ..
            }
            | Self::InvariantReceiverInsert {
                aggregate, value, ..
            }
            | Self::TaskCarrierUpdate {
                aggregate, value, ..
            } => vec![*aggregate, *value],
            Self::SumConstruct { payload, .. } => payload.to_vec(),
            Self::TextConcat { left, right }
            | Self::TextCompare { left, right, .. }
            | Self::BytesAppend { left, right }
            | Self::BytesCompare { left, right, .. }
            | Self::BoolCompare { left, right, .. }
            | Self::FloatBinary { left, right, .. }
            | Self::IntCompare { left, right, .. }
            | Self::FloatCompare { left, right, .. } => vec![*left, *right],
            Self::PathJoin { base, child, .. } => vec![*base, *child],
            Self::ListAppend { list, value } | Self::ListAppendUnique { list, value } => {
                vec![*list, *value]
            }
            Self::ListGet { list, index } => vec![*list, *index],
            Self::TextMapInsert { map, key, value } => vec![*map, *key, *value],
            Self::TextMapConstructEntries { entries } => vec![*entries],
            Self::TextMapLength { map } => vec![*map],
            Self::TextMapContains { map, key }
            | Self::TextMapGet { map, key }
            | Self::TextMapRemove { map, key } => vec![*map, *key],
            Self::TextMapEntryGet { map, index } => vec![*map, *index],
            Self::TaskCreate { arguments, .. }
            | Self::IoTaskCreate { arguments, .. }
            | Self::DirectCall { arguments, .. } => arguments.to_vec(),
            Self::TaskJoin { tasks, .. } => tasks.to_vec(),
            Self::TaskJoinList { tasks, .. } => vec![*tasks],
            Self::TaskOutcomeTake { task } => vec![*task],
            Self::IntSuccessorBelow {
                value,
                upper_bound,
                proof,
            } => vec![*value, *upper_bound, *proof],
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockTarget {
    pub block: BlockId,
    pub arguments: Box<[ValueId]>,
}

impl BlockTarget {
    #[must_use]
    pub fn new(block: BlockId, arguments: impl Into<Box<[ValueId]>>) -> Self {
        Self {
            block,
            arguments: arguments.into(),
        }
    }
}

/// One exhaustive closed-sum case edge.
///
/// [`TerminatorKind::SumSwitch`] injects the selected owned variant payload
/// into the destination's leading block parameters.
/// [`TerminatorKind::SumBorrowSwitch`] injects borrowed Task-bearing payload
/// aliases instead. [`TerminatorKind::SumZipSwitch`] injects the left payload
/// followed by the right payload. Explicit `arguments` are forwarded after
/// those implicit values, mirroring result edges without materializing
/// payload values in the source block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SumCase {
    pub variant: u32,
    pub block: BlockId,
    pub arguments: Box<[ValueId]>,
}

impl SumCase {
    #[must_use]
    pub fn new(variant: u32, block: BlockId, arguments: impl Into<Box<[ValueId]>>) -> Self {
        Self {
            variant,
            block,
            arguments: arguments.into(),
        }
    }
}

/// A normal edge which defines one or more operation results only in its
/// destination.
///
/// Implicit results are injected into the leading destination parameters.
/// Explicit `arguments` are forwarded to the remaining destination
/// parameters. There is intentionally no result [`ValueId`] in the source
/// block which could be used by a fault edge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResultTarget {
    pub block: BlockId,
    pub arguments: Box<[ValueId]>,
}

impl ResultTarget {
    #[must_use]
    pub fn new(block: BlockId, arguments: impl Into<Box<[ValueId]>>) -> Self {
        Self {
            block,
            arguments: arguments.into(),
        }
    }
}

/// An edge entered with a source fault active.
///
/// Fault identity and diagnostics live in the runtime fault context rather
/// than in an ordinary SSA value. Explicit arguments are therefore only the
/// values forwarded to destination block parameters. When the source block
/// was inactive, this edge activates the operation's fault as the primary
/// fault. During active cleanup, the existing fault remains primary, the new
/// cleanup fault is suppressed, and the edge stays active so remaining cleanup
/// can continue before [`TerminatorKind::ResumeFault`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnwindTarget {
    pub block: BlockId,
    pub arguments: Box<[ValueId]>,
}

impl UnwindTarget {
    #[must_use]
    pub fn new(block: BlockId, arguments: impl Into<Box<[ValueId]>>) -> Self {
        Self {
            block,
            arguments: arguments.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FaultCode {
    ArtifactProofRejected,
    IntegerOverflow,
    IntegerDivisionByZero,
    IntegerDivisionOverflow,
    InvalidByte,
    InvalidDuration,
    InvalidSleepDuration,
    SleepDurationOverflow,
    TaskAnyFailed,
    EmptyTaskJoin,
    LogWrite,
    StdoutWrite,
}

/// Structured join semantics for one directly awaited fixed child set.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum AwaitMode {
    /// Completes only when every child succeeds and injects every result in
    /// child order.
    All,
    /// Completes after the first successful child and injects only that
    /// winner's result. All fixed children must have one common output type.
    Any,
    /// Completes only after every child reaches a terminal state and injects
    /// every terminal child handle in source order. Each handle must then be
    /// consumed by [`InstructionKind::TaskOutcomeTake`].
    Settled,
    /// Completes after the first terminal child, cancels and drains every
    /// loser, and injects only the terminal winner handle. All fixed children
    /// must have one common output type, and the winner must then be consumed
    /// by [`InstructionKind::TaskOutcomeTake`].
    Race,
}

/// Ordered variants consumed by [`InstructionKind::TaskOutcomeTake`]. Checked
/// MIR establishes the source definition identities; independent LCIR
/// validation rechecks their concrete shapes through [`CanonicalTypeCatalog`].
pub const TASK_OUTCOME_COMPLETED_VARIANT: u32 = 0;
pub const TASK_OUTCOME_FAULTED_VARIANT: u32 = 1;
pub const TASK_OUTCOME_CANCELLED_VARIANT: u32 = 2;

/// Closed runtime-owned I/O leaves admitted by direct LCIR.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum IoTaskOperation {
    FileOpenRead,
    FileCreate,
    FileReadText,
    FileWriteText,
    SocketConnect,
    SocketReadText,
    SocketWriteText,
}

/// Source-visible treatment of an ordinary host I/O error.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum IoTaskErrorMode {
    /// Completes with the exact `Result[T, IoError]` value.
    Result,
    /// Records an operation-specific runtime fault and faults `Task[T]`.
    Fault,
}

/// Statically known external-resource class for typed lexical disposal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceKind {
    File,
    Socket,
}

/// Per-field UTF-8 byte budget for compiler-private contract-fault text.
///
/// Independent validation applies the limit to both the optional user code
/// and the canonical diagnostic message before dumps or native backends may
/// encode either value.
pub const CONTRACT_FAULT_TEXT_MAX_BYTES: usize = 4 * 1024;

/// Stable source-level contract-fault category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContractFaultKind {
    Precondition,
    Postcondition,
    Invariant,
    Assertion,
}

impl ContractFaultKind {
    #[must_use]
    pub const fn fault_code(self) -> &'static str {
        match self {
            Self::Precondition => "PreconditionFault",
            Self::Postcondition => "PostconditionFault",
            Self::Invariant => "InvariantFault",
            Self::Assertion => "AssertionFault",
        }
    }

    #[must_use]
    pub const fn category(self) -> &'static str {
        match self {
            Self::Precondition => "precondition",
            Self::Postcondition => "postcondition",
            Self::Invariant => "invariant",
            Self::Assertion => "assertion",
        }
    }
}

/// Source location blamed when a contract fails.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContractFaultBlame {
    /// A span known statically in the current LCIR function.
    Static(Span),
    /// The source span carried from the `TaskCreate` instruction which
    /// instantiated the current coroutine.
    CoroutineCallSite,
}

impl From<Span> for ContractFaultBlame {
    fn from(span: Span) -> Self {
        Self::Static(span)
    }
}

/// Complete diagnostic identity for one source contract-fault origin.
///
/// A synchronous precondition stores its exact closed-world call-site span.
/// An asynchronous precondition instead names the compiler-private call-site
/// span carried in its coroutine frame. All other kinds use their contract or
/// assertion span. Independent validation checks those canonical relationships
/// and the current language-defined message schema.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractFaultMetadata {
    kind: ContractFaultKind,
    user_code: Option<String>,
    message: String,
    contract_span: Span,
    blame: ContractFaultBlame,
}

impl ContractFaultMetadata {
    /// Creates unchecked metadata for the raw LCIR builder. The independent
    /// validator rejects an inconsistent kind, user code, message, or span.
    #[must_use]
    pub fn new(
        kind: ContractFaultKind,
        user_code: Option<String>,
        message: impl Into<String>,
        contract_span: Span,
        blame: impl Into<ContractFaultBlame>,
    ) -> Self {
        Self {
            kind,
            user_code,
            message: message.into(),
            contract_span,
            blame: blame.into(),
        }
    }

    /// Constructs the canonical payload for a named source contract.
    #[must_use]
    pub fn contract(
        kind: ContractFaultKind,
        user_code: impl Into<String>,
        contract_span: Span,
        blame: impl Into<ContractFaultBlame>,
    ) -> Self {
        let user_code = user_code.into();
        let message = format!("contract `{user_code}` was not satisfied");
        Self::new(kind, Some(user_code), message, contract_span, blame)
    }

    /// Constructs the canonical payload for a precondition checked inside an
    /// asynchronous callee. The exact blame span is supplied by the
    /// coroutine's `TaskCreate` call site at run time.
    #[must_use]
    pub fn coroutine_precondition(user_code: impl Into<String>, contract_span: Span) -> Self {
        Self::contract(
            ContractFaultKind::Precondition,
            user_code,
            contract_span,
            ContractFaultBlame::CoroutineCallSite,
        )
    }

    /// Constructs the canonical payload for a source `assert` statement.
    #[must_use]
    pub fn assertion(span: Span) -> Self {
        Self::new(
            ContractFaultKind::Assertion,
            None,
            "assertion was not satisfied",
            span,
            span,
        )
    }

    #[must_use]
    pub const fn kind(&self) -> ContractFaultKind {
        self.kind
    }

    #[must_use]
    pub fn user_code(&self) -> Option<&str> {
        self.user_code.as_deref()
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub const fn contract_span(&self) -> Span {
        self.contract_span
    }

    #[must_use]
    pub const fn blame(&self) -> ContractFaultBlame {
        self.blame
    }

    /// Returns the statically known blame span, or `None` when a coroutine
    /// obtains the span from its creating call at run time.
    #[must_use]
    pub const fn blame_span(&self) -> Option<Span> {
        match self.blame {
            ContractFaultBlame::Static(span) => Some(span),
            ContractFaultBlame::CoroutineCallSite => None,
        }
    }
}

/// Complete typed identity for an explicitly originated fault.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FaultMetadata {
    Runtime(FaultCode),
    Contract(ContractFaultMetadata),
}

impl FaultMetadata {
    #[must_use]
    pub const fn runtime(code: FaultCode) -> Self {
        Self::Runtime(code)
    }

    #[must_use]
    pub const fn contract(metadata: ContractFaultMetadata) -> Self {
        Self::Contract(metadata)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Terminator {
    pub(crate) kind: TerminatorKind,
    pub(crate) origin: Origin,
    pub(crate) writebacks: Box<[ValueId]>,
}

impl Terminator {
    #[must_use]
    pub fn new(kind: TerminatorKind, origin: Origin) -> Self {
        Self {
            kind,
            origin,
            writebacks: Box::new([]),
        }
    }

    /// Constructs a terminal return/fault operation carrying the current
    /// values of all signature inout parameters.
    #[must_use]
    pub fn with_writebacks(
        kind: TerminatorKind,
        origin: Origin,
        writebacks: impl Into<Box<[ValueId]>>,
    ) -> Self {
        Self {
            kind,
            origin,
            writebacks: writebacks.into(),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> &TerminatorKind {
        &self.kind
    }

    #[must_use]
    pub const fn origin(&self) -> Origin {
        self.origin
    }

    #[must_use]
    pub const fn writebacks(&self) -> &[ValueId] {
        &self.writebacks
    }

    pub(crate) fn operands(&self) -> Vec<ValueId> {
        let mut operands = self.kind.operands();
        operands.extend_from_slice(&self.writebacks);
        operands
    }

    pub(crate) fn control_flow_edges(&self) -> Vec<ControlFlowEdge> {
        self.kind.control_flow_edges()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ControlFlowEdge {
    pub block: BlockId,
    /// Sets the destination state to active without replacing an already
    /// active primary fault.
    pub activates_fault: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TerminatorKind {
    Jump(BlockTarget),
    Branch {
        condition: ValueId,
        then_target: BlockTarget,
        else_target: BlockTarget,
    },
    /// Exhaustively switches over a closed sum. Checked LCIR requires exactly
    /// one ordered case for every variant; each edge defines that variant's
    /// payload in its destination block parameters.
    SumSwitch {
        scrutinee: ValueId,
        cases: Box<[SumCase]>,
    },
    /// Exhaustively observes a closed Task-bearing sum without consuming it.
    /// Selected Task-bearing payload parameters are borrowed aliases, while
    /// explicit edge arguments retain their ordinary move semantics.
    SumBorrowSwitch {
        scrutinee: ValueId,
        cases: Box<[SumCase]>,
    },
    /// Compares two values with the same closed-sum type and dispatches once.
    /// A matching tag selects the ordered case and injects the left payload
    /// followed by the right payload. Different tags take `mismatch` without
    /// exposing either inactive payload. Task-bearing sums are not valid here
    /// because the mismatch edge would discard affine values.
    SumZipSwitch {
        left: ValueId,
        right: ValueId,
        cases: Box<[SumCase]>,
        mismatch: BlockTarget,
    },
    /// Exhaustively switches over one artifact-closed dynamic candidate set.
    /// Each edge injects the selected concrete payload as its leading block
    /// parameter, then forwards the explicit case arguments.
    DynSwitch {
        scrutinee: ValueId,
        cases: Box<[SumCase]>,
    },
    Return(ValueId),
    /// Constructs one typed `Task[Unit]` backed by the executor's monotonic
    /// timer reactor. `milliseconds` is the normalized signed Int payload of
    /// either a source Int or Duration. The task handle exists only on
    /// `normal`; a negative duration or checked timer-range overflow activates
    /// the corresponding source fault and enters `fault`.
    TaskSleep {
        milliseconds: ValueId,
        normal: ResultTarget,
        fault: UnwindTarget,
    },
    /// Consumes one or more structured child Tasks. The initial callback
    /// invocation stores `normal.arguments`, attaches all children to one
    /// exact mode-specific join, and returns pending when needed. Resume state
    /// `state` injects completed values (`all`/`any`) or terminal child handles
    /// (`settled`/`race`) into the leading normal parameters before the
    /// forwarded values. Terminal handles remain affine and are converted to
    /// canonical outcomes only by explicit collecting `task.outcome_take`
    /// instructions. `fault` and `cancel` receive the same forwarded live row
    /// without child values, allowing lexical cleanup to run before the
    /// propagated terminal outcome.
    AwaitTasks {
        state: u32,
        mode: AwaitMode,
        tasks: Box<[ValueId]>,
        normal: ResultTarget,
        fault: UnwindTarget,
        cancel: BlockTarget,
    },
    /// Negates a signed integer, producing its value only on `normal` and
    /// activating [`FaultCode::IntegerOverflow`] on `fault`.
    CheckedIntNegate {
        value: ValueId,
        normal: ResultTarget,
        fault: UnwindTarget,
    },
    /// Computes checked signed arithmetic, producing its value only on
    /// `normal` and activating the operation-specific source fault on `fault`.
    CheckedIntBinary {
        op: CheckedIntBinaryOp,
        left: ValueId,
        right: ValueId,
        normal: ResultTarget,
        fault: UnwindTarget,
    },
    /// Calls an exactly may-fault function. Its return value exists only on
    /// `normal`; `unwind` is entered with the callee's fault still active.
    Invoke {
        callee: InstanceId,
        arguments: Box<[ValueId]>,
        normal: ResultTarget,
        unwind: UnwindTarget,
    },
    /// Writes one structured JSON log line through the direct Text ABI.
    /// `fields` is the canonical managed `TextMap[Text]` value. A successful
    /// operation defines Unit only on `normal`; a device failure activates
    /// [`FaultCode::LogWrite`] and enters `fault` so lexical cleanup can run
    /// before propagation.
    LogWrite {
        level: ValueId,
        message: ValueId,
        fields: ValueId,
        normal: ResultTarget,
        fault: UnwindTarget,
    },
    /// Writes one exact Text value to the process standard-output stream.
    /// A successful operation defines Unit only on `normal`; a device failure
    /// activates [`FaultCode::StdoutWrite`] and enters `fault` so lexical
    /// cleanup can run before propagation.
    StdoutWrite {
        text: ValueId,
        normal: ResultTarget,
        fault: UnwindTarget,
    },
    /// Continues through `success` when true or activates the checked fault
    /// metadata and enters `fault` when false.
    Assert {
        condition: ValueId,
        metadata: FaultMetadata,
        success: BlockTarget,
        fault: UnwindTarget,
    },
    /// Originates and reports a source fault at an inactive terminal boundary.
    Fault {
        metadata: FaultMetadata,
    },
    /// Propagates the already active source fault without reporting it again.
    /// This is not a local `MAY_FAULT` source: the checked operation, assertion,
    /// terminal fault, or transitively faulting invoke which made the path
    /// active supplies that effect.
    ResumeFault,
    /// Completes an active coroutine cancellation after its non-suspending
    /// lexical cleanup suffix has run. This terminal is invalid in ordinary
    /// synchronous functions and while a source fault is active.
    TaskCancelled,
}

impl TerminatorKind {
    #[allow(clippy::too_many_lines)]
    pub(crate) fn operands(&self) -> Vec<ValueId> {
        match self {
            Self::Jump(target) => target.arguments.to_vec(),
            Self::Branch {
                condition,
                then_target,
                else_target,
            } => {
                let mut operands = Vec::with_capacity(
                    1 + then_target.arguments.len() + else_target.arguments.len(),
                );
                operands.push(*condition);
                operands.extend_from_slice(&then_target.arguments);
                operands.extend_from_slice(&else_target.arguments);
                operands
            }
            Self::SumSwitch { scrutinee, cases }
            | Self::SumBorrowSwitch { scrutinee, cases }
            | Self::DynSwitch { scrutinee, cases } => {
                let mut operands = Vec::with_capacity(
                    1 + cases.iter().map(|case| case.arguments.len()).sum::<usize>(),
                );
                operands.push(*scrutinee);
                for case in cases {
                    operands.extend_from_slice(&case.arguments);
                }
                operands
            }
            Self::SumZipSwitch {
                left,
                right,
                cases,
                mismatch,
            } => {
                let mut operands = Vec::with_capacity(
                    2 + mismatch.arguments.len()
                        + cases.iter().map(|case| case.arguments.len()).sum::<usize>(),
                );
                operands.push(*left);
                operands.push(*right);
                for case in cases {
                    operands.extend_from_slice(&case.arguments);
                }
                operands.extend_from_slice(&mismatch.arguments);
                operands
            }
            Self::Return(value) => vec![*value],
            Self::TaskSleep {
                milliseconds,
                normal,
                fault,
            } => {
                let mut operands =
                    Vec::with_capacity(1 + normal.arguments.len() + fault.arguments.len());
                operands.push(*milliseconds);
                operands.extend_from_slice(&normal.arguments);
                operands.extend_from_slice(&fault.arguments);
                operands
            }
            Self::AwaitTasks {
                tasks,
                normal,
                fault,
                cancel,
                ..
            } => {
                let mut operands = Vec::with_capacity(
                    tasks.len()
                        + normal.arguments.len()
                        + fault.arguments.len()
                        + cancel.arguments.len(),
                );
                operands.extend_from_slice(tasks);
                operands.extend_from_slice(&normal.arguments);
                operands.extend_from_slice(&fault.arguments);
                operands.extend_from_slice(&cancel.arguments);
                operands
            }
            Self::CheckedIntNegate {
                value,
                normal,
                fault,
            } => {
                let mut operands =
                    Vec::with_capacity(1 + normal.arguments.len() + fault.arguments.len());
                operands.push(*value);
                operands.extend_from_slice(&normal.arguments);
                operands.extend_from_slice(&fault.arguments);
                operands
            }
            Self::CheckedIntBinary {
                left,
                right,
                normal,
                fault,
                ..
            } => {
                let mut operands =
                    Vec::with_capacity(2 + normal.arguments.len() + fault.arguments.len());
                operands.push(*left);
                operands.push(*right);
                operands.extend_from_slice(&normal.arguments);
                operands.extend_from_slice(&fault.arguments);
                operands
            }
            Self::Invoke {
                arguments,
                normal,
                unwind,
                ..
            } => {
                let mut operands = Vec::with_capacity(
                    arguments.len() + normal.arguments.len() + unwind.arguments.len(),
                );
                operands.extend_from_slice(arguments);
                operands.extend_from_slice(&normal.arguments);
                operands.extend_from_slice(&unwind.arguments);
                operands
            }
            Self::LogWrite {
                level,
                message,
                fields,
                normal,
                fault,
            } => {
                let mut operands =
                    Vec::with_capacity(3 + normal.arguments.len() + fault.arguments.len());
                operands.push(*level);
                operands.push(*message);
                operands.push(*fields);
                operands.extend_from_slice(&normal.arguments);
                operands.extend_from_slice(&fault.arguments);
                operands
            }
            Self::StdoutWrite {
                text,
                normal,
                fault,
            } => {
                let mut operands =
                    Vec::with_capacity(1 + normal.arguments.len() + fault.arguments.len());
                operands.push(*text);
                operands.extend_from_slice(&normal.arguments);
                operands.extend_from_slice(&fault.arguments);
                operands
            }
            Self::Assert {
                condition,
                success,
                fault,
                ..
            } => {
                let mut operands =
                    Vec::with_capacity(1 + success.arguments.len() + fault.arguments.len());
                operands.push(*condition);
                operands.extend_from_slice(&success.arguments);
                operands.extend_from_slice(&fault.arguments);
                operands
            }
            Self::Fault { .. } | Self::ResumeFault | Self::TaskCancelled => Vec::new(),
        }
    }

    pub(crate) fn control_flow_edges(&self) -> Vec<ControlFlowEdge> {
        let preserve = |block| ControlFlowEdge {
            block,
            activates_fault: false,
        };
        let activate = |block| ControlFlowEdge {
            block,
            activates_fault: true,
        };
        match self {
            Self::Jump(target) => vec![preserve(target.block)],
            Self::Branch {
                then_target,
                else_target,
                ..
            } => vec![preserve(then_target.block), preserve(else_target.block)],
            Self::SumSwitch { cases, .. }
            | Self::SumBorrowSwitch { cases, .. }
            | Self::DynSwitch { cases, .. } => {
                cases.iter().map(|case| preserve(case.block)).collect()
            }
            Self::SumZipSwitch {
                cases, mismatch, ..
            } => {
                let mut edges = cases
                    .iter()
                    .map(|case| preserve(case.block))
                    .collect::<Vec<_>>();
                edges.push(preserve(mismatch.block));
                edges
            }
            Self::AwaitTasks {
                normal,
                fault,
                cancel,
                ..
            } => vec![
                preserve(normal.block),
                activate(fault.block),
                preserve(cancel.block),
            ],
            Self::TaskSleep { normal, fault, .. }
            | Self::CheckedIntNegate { normal, fault, .. }
            | Self::CheckedIntBinary { normal, fault, .. }
            | Self::LogWrite { normal, fault, .. }
            | Self::StdoutWrite { normal, fault, .. } => {
                vec![preserve(normal.block), activate(fault.block)]
            }
            Self::Invoke { normal, unwind, .. } => {
                vec![preserve(normal.block), activate(unwind.block)]
            }
            Self::Assert { success, fault, .. } => {
                vec![preserve(success.block), activate(fault.block)]
            }
            Self::Return(_) | Self::Fault { .. } | Self::ResumeFault | Self::TaskCancelled => {
                Vec::new()
            }
        }
    }
}
