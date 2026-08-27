use std::fmt;

use loom_core::Span;
use loom_mir::{ExprId as MirExprId, FunctionId as MirFunctionId};

use crate::ids::ProgramBrand;
use crate::{
    BlockId, InstanceId, InstanceKey, InstancePlan, InstructionId, RepresentationPlan, ValueId,
    ValueTypeId,
};

/// A target-specific LCIR program before or after independent validation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Program {
    pub(crate) brand: ProgramBrand,
    pub(crate) representations: RepresentationPlan,
    pub(crate) instances: InstancePlan,
    pub(crate) functions: Vec<Function>,
}

impl Program {
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
    /// in range, and backed by direct product values.
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
pub const LIST_LITERAL_MAX_ELEMENTS: usize = 4096;

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
    /// Produces a new product value by replacing one field. The input remains
    /// independently usable, preserving source copy semantics in SSA.
    ProductInsert {
        aggregate: ValueId,
        field: u32,
        value: ValueId,
    },
    /// Rebuilds a mutable receiver whose declared record invariant is checked
    /// at the function boundary. This is a checked-MIR-only operation: the
    /// intermediate SSA value is not an independently established invariant
    /// proof and may leave the function only after the exit invariant passes.
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
    /// Constructs one closed sum variant. The result type selects the sum
    /// representation and `variant` selects its ordered payload signature.
    SumConstruct {
        variant: u32,
        payload: Box<[ValueId]>,
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
            Self::Constant(_) | Self::TextLiteral { .. } => Vec::new(),
            Self::TextLength { text } => vec![*text],
            Self::TextGet { text, index, .. } => vec![*text, *index],
            Self::ListConstruct { elements } => elements.to_vec(),
            Self::ListLength { list } => vec![*list],
            Self::TextContains { text, needle } => vec![*text, *needle],
            Self::ProductConstruct { fields } | Self::InvariantRecordProven { fields } => {
                fields.to_vec()
            }
            Self::ProductExtract { aggregate, .. } => vec![*aggregate],
            Self::ProductInsert {
                aggregate, value, ..
            }
            | Self::InvariantReceiverInsert {
                aggregate, value, ..
            } => vec![*aggregate, *value],
            Self::RefineProven { value }
            | Self::Unrefine { value }
            | Self::BoolNot { value }
            | Self::FloatNegate { value } => vec![*value],
            Self::SumConstruct { payload, .. } => payload.to_vec(),
            Self::TextConcat { left, right }
            | Self::TextCompare { left, right, .. }
            | Self::BoolCompare { left, right, .. }
            | Self::FloatBinary { left, right, .. }
            | Self::IntCompare { left, right, .. }
            | Self::FloatCompare { left, right, .. } => vec![*left, *right],
            Self::ListAppend { list, value } | Self::ListAppendUnique { list, value } => {
                vec![*list, *value]
            }
            Self::ListGet { list, index } => vec![*list, *index],
            Self::IntSuccessorBelow {
                value,
                upper_bound,
                proof,
            } => vec![*value, *upper_bound, *proof],
            Self::DirectCall { arguments, .. } => arguments.to_vec(),
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
/// The selected variant payload is injected into the destination's leading
/// block parameters. Explicit `arguments` are forwarded after that implicit
/// payload, mirroring result edges without materializing payload values in the
/// source block.
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

/// A normal edge which defines an operation result only in its destination.
///
/// The implicit result is injected into destination parameter zero. Explicit
/// `arguments` are forwarded to the remaining destination parameters. There
/// is intentionally no result [`ValueId`] in the source block which could be
/// used by a fault edge.
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
    ResourceClose,
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

/// Complete diagnostic identity for one source contract-fault origin.
///
/// LCIR stores concrete spans. A precondition check therefore carries the
/// exact closed-world call-site span as `blame_span`; all other kinds use their
/// contract or assertion span. Independent validation checks those canonical
/// relationships and the current language-defined message schema.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractFaultMetadata {
    kind: ContractFaultKind,
    user_code: Option<String>,
    message: String,
    contract_span: Span,
    blame_span: Span,
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
        blame_span: Span,
    ) -> Self {
        Self {
            kind,
            user_code,
            message: message.into(),
            contract_span,
            blame_span,
        }
    }

    /// Constructs the canonical payload for a named source contract.
    #[must_use]
    pub fn contract(
        kind: ContractFaultKind,
        user_code: impl Into<String>,
        contract_span: Span,
        blame_span: Span,
    ) -> Self {
        let user_code = user_code.into();
        let message = format!("contract `{user_code}` was not satisfied");
        Self::new(kind, Some(user_code), message, contract_span, blame_span)
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
    pub const fn blame_span(&self) -> Span {
        self.blame_span
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
    Return(ValueId),
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
    /// Closes one compiler-known File or Socket handle without a universal
    /// value envelope. The operation functionally writes the closed resource
    /// back on both edges so lexical disposal preserves exact inout state.
    ResourceClose {
        kind: ResourceKind,
        resource: ValueId,
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
}

impl TerminatorKind {
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
            Self::SumSwitch { scrutinee, cases } => {
                let mut operands = Vec::with_capacity(
                    1 + cases.iter().map(|case| case.arguments.len()).sum::<usize>(),
                );
                operands.push(*scrutinee);
                for case in cases {
                    operands.extend_from_slice(&case.arguments);
                }
                operands
            }
            Self::Return(value) => vec![*value],
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
            Self::ResourceClose {
                resource,
                normal,
                fault,
                ..
            } => {
                let mut operands =
                    Vec::with_capacity(1 + normal.arguments.len() + fault.arguments.len());
                operands.push(*resource);
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
            Self::Fault { .. } | Self::ResumeFault => Vec::new(),
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
            Self::SumSwitch { cases, .. } => {
                cases.iter().map(|case| preserve(case.block)).collect()
            }
            Self::CheckedIntNegate { normal, fault, .. }
            | Self::CheckedIntBinary { normal, fault, .. }
            | Self::ResourceClose { normal, fault, .. } => {
                vec![preserve(normal.block), activate(fault.block)]
            }
            Self::Invoke { normal, unwind, .. } => {
                vec![preserve(normal.block), activate(unwind.block)]
            }
            Self::Assert { success, fault, .. } => {
                vec![preserve(success.block), activate(fault.block)]
            }
            Self::Return(_) | Self::Fault { .. } | Self::ResumeFault => Vec::new(),
        }
    }
}
