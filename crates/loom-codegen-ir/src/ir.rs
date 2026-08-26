use loom_core::Span;
use loom_mir::{ExprId as MirExprId, FunctionId as MirFunctionId};

use crate::ids::ProgramBrand;
use crate::{BlockId, InstanceId, InstructionId, RepresentationPlan, ValueId, ValueTypeId};

/// A target-specific LCIR program before or after independent validation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Program {
    pub(crate) brand: ProgramBrand,
    pub(crate) representations: RepresentationPlan,
    pub(crate) functions: Vec<Function>,
}

impl Program {
    #[must_use]
    pub const fn representations(&self) -> &RepresentationPlan {
        &self.representations
    }

    #[must_use]
    pub fn functions(&self) -> &[Function] {
        &self.functions
    }

    #[must_use]
    pub fn function(&self, id: InstanceId) -> Option<&Function> {
        (id.brand() == self.brand)
            .then(|| self.functions.get(id.index()))
            .flatten()
    }
}

/// Transitive runtime behavior represented by a lowered function.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Effects(u8);

impl Effects {
    const MAY_FAULT_BIT: u8 = 1;

    pub const NONE: Self = Self(0);
    pub const MAY_FAULT: Self = Self(Self::MAY_FAULT_BIT);

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
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Signature {
    params: Box<[ValueTypeId]>,
    result: ValueTypeId,
}

impl Signature {
    #[must_use]
    pub fn new(params: impl Into<Box<[ValueTypeId]>>, result: ValueTypeId) -> Self {
        Self {
            params: params.into(),
            result,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstructionKind {
    Constant(Constant),
    BoolNot {
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
    FloatCompare {
        predicate: FloatPredicate,
        left: ValueId,
        right: ValueId,
    },
    /// Foundation calls are deliberately limited to infallible scalar
    /// callees. Fallible calls need an explicit invoke/status ABI in the next
    /// vertical slice.
    DirectCall {
        callee: InstanceId,
        arguments: Box<[ValueId]>,
    },
}

impl InstructionKind {
    pub(crate) fn operands(&self) -> Vec<ValueId> {
        match self {
            Self::Constant(_) => Vec::new(),
            Self::BoolNot { value } => vec![*value],
            Self::FloatBinary { left, right, .. }
            | Self::IntCompare { left, right, .. }
            | Self::FloatCompare { left, right, .. } => vec![*left, *right],
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FaultCode {
    IntegerOverflow,
    IntegerDivisionByZero,
    IntegerDivisionOverflow,
    AssertionFailed,
    ContractFailed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Terminator {
    pub(crate) kind: TerminatorKind,
    pub(crate) origin: Origin,
}

impl Terminator {
    #[must_use]
    pub const fn new(kind: TerminatorKind, origin: Origin) -> Self {
        Self { kind, origin }
    }

    #[must_use]
    pub const fn kind(&self) -> &TerminatorKind {
        &self.kind
    }

    #[must_use]
    pub const fn origin(&self) -> Origin {
        self.origin
    }

    pub(crate) fn operands(&self) -> Vec<ValueId> {
        self.kind.operands()
    }

    pub(crate) fn targets(&self) -> Vec<&BlockTarget> {
        self.kind.targets()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TerminatorKind {
    Jump(BlockTarget),
    Branch {
        condition: ValueId,
        then_target: BlockTarget,
        else_target: BlockTarget,
    },
    Return(ValueId),
    Fault {
        code: FaultCode,
    },
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
            Self::Return(value) => vec![*value],
            Self::Fault { .. } => Vec::new(),
        }
    }

    pub(crate) fn targets(&self) -> Vec<&BlockTarget> {
        match self {
            Self::Jump(target) => vec![target],
            Self::Branch {
                then_target,
                else_target,
                ..
            } => vec![then_target, else_target],
            Self::Return(_) | Self::Fault { .. } => Vec::new(),
        }
    }
}
