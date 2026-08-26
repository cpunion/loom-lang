//! Typed executable IR with explicit contract and dispatch operations.

mod artifact;
mod liveness;
mod validation;

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use loom_core::Span;
use serde::{Deserialize, Serialize};

pub use artifact::{
    ArtifactError, INTERPRETED_ARTIFACT_FORMAT, INTERPRETED_ARTIFACT_VERSION,
    LOOM_LANGUAGE_VERSION, decode_interpreted_artifact, decode_interpreted_executable_artifact,
    encode_interpreted_artifact, encode_interpreted_executable_artifact,
};
pub use liveness::analyze_suspension_liveness;
pub use validation::{
    CheckedProgram, MirValidationCode, MirValidationError, MirValidationErrors, check_program,
    validate_program,
};

macro_rules! id_type {
    ($name:ident) => {
        #[derive(
            Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub u32);
    };
}

id_type!(TypeId);
id_type!(FunctionId);
id_type!(LocalId);
id_type!(ExprId);
id_type!(VariantId);
id_type!(ConceptId);
id_type!(RequirementId);
id_type!(WitnessId);

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Program {
    pub types: Vec<TypeDef>,
    /// Compiler-private concept metadata, indexed directly by [`ConceptId`].
    #[serde(default)]
    pub concepts: Vec<ConceptDef>,
    /// Compiler-private method metadata, indexed directly by [`RequirementId`].
    #[serde(default)]
    pub requirements: Vec<RequirementDef>,
    pub functions: Vec<Function>,
    pub witnesses: Vec<Witness>,
    pub tests: Vec<FunctionId>,
    pub exports: BTreeMap<String, FunctionId>,
    /// Compiler-known prelude identities. Entries remain optional so focused
    /// MIR tests can construct programs which do not exercise that facility.
    #[serde(default)]
    pub prelude: PreludeIds,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PreludeIds {
    pub result: Option<TypeId>,
    pub option: Option<TypeId>,
    pub constraint_error: Option<TypeId>,
    pub parse_float_error: Option<TypeId>,
    pub parse_int_error: Option<TypeId>,
    pub task_fault: Option<TypeId>,
    pub task_outcome: Option<TypeId>,
    pub duration: Option<TypeId>,
    pub file: Option<TypeId>,
    pub socket: Option<TypeId>,
    #[serde(default)]
    pub bytes: Option<TypeId>,
    #[serde(default)]
    pub path: Option<TypeId>,
    #[serde(default)]
    pub decode_text_error: Option<TypeId>,
    #[serde(default)]
    pub path_error: Option<TypeId>,
    #[serde(default)]
    pub text_map: Option<TypeId>,
    #[serde(default)]
    pub json: Option<TypeId>,
    #[serde(default)]
    pub json_error: Option<TypeId>,
    #[serde(default)]
    pub io_error: Option<TypeId>,
    #[serde(default)]
    pub io_error_kind: Option<TypeId>,
    #[serde(default)]
    pub log_level: Option<TypeId>,
}

impl Program {
    #[must_use]
    pub fn function(&self, id: FunctionId) -> Option<&Function> {
        self.functions.get(id.0 as usize)
    }

    #[must_use]
    pub fn type_def(&self, id: TypeId) -> Option<&TypeDef> {
        self.types.get(id.0 as usize)
    }

    #[must_use]
    pub fn concept(&self, id: ConceptId) -> Option<&ConceptDef> {
        self.concepts.get(id.0 as usize)
    }

    #[must_use]
    pub fn requirement(&self, id: RequirementId) -> Option<&RequirementDef> {
        self.requirements.get(id.0 as usize)
    }

    #[must_use]
    pub fn witness(&self, id: WitnessId) -> Option<&Witness> {
        self.witnesses.get(id.0 as usize)
    }

    /// Validates every index and executable type shape in this MIR program.
    ///
    /// # Errors
    ///
    /// Returns all independently discoverable validation failures.
    pub fn validate(&self) -> Result<(), MirValidationErrors> {
        validate_program(self)
    }

    /// Consumes this unchecked program and returns the checked boundary type.
    ///
    /// # Errors
    ///
    /// Returns all independently discoverable validation failures.
    pub fn into_checked(self) -> Result<CheckedProgram, MirValidationErrors> {
        check_program(self)
    }

    /// Reassigns every executable expression a canonical, function-local id.
    ///
    /// Functions are independent identity domains. Within each function, ids
    /// follow deterministic preorder over the body and form the dense range
    /// `0..expression_count`.
    ///
    /// # Errors
    ///
    /// Returns an error if one function exhausts the usable [`ExprId`] range;
    /// [`u32::MAX`] remains permanently reserved for [`ExprId::UNASSIGNED`].
    pub fn renumber_expr_ids(&mut self) -> Result<(), ExprIdOverflow> {
        for function in &mut self.functions {
            function.renumber_expr_ids()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum Type {
    /// Internal control-flow bottom; no runtime value can inhabit this type.
    Never,
    Unit,
    Bool,
    Int,
    Float,
    Text,
    Tuple(Vec<Type>),
    List(Box<Type>),
    Nominal(TypeId, Vec<Type>),
    Parameter(u32),
    /// Projection through a function's witness parameter. The witness index is
    /// stable within that function's frame; the associated key is nominal.
    AssociatedProjection {
        witness: u32,
        associated: String,
    },
    /// Compiler-managed, affine handle produced by an async constructor.
    Task(Box<Type>),
    TaskOutcome(Box<Type>),
    View {
        mutable: bool,
        concept: ConceptId,
        bindings: BTreeMap<String, Type>,
    },
    Error,
}

/// A concept requirement's compiler-private type schema.
///
/// `SelfType` and `Associated` are substituted by a witness before a call can
/// enter executable MIR. `MethodParameter` is alpha-normalized within one
/// requirement and is instantiated from a static call's explicit type arguments.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RequirementType {
    Unit,
    Bool,
    Int,
    Float,
    Text,
    Tuple(Vec<RequirementType>),
    Nominal(TypeId, Vec<RequirementType>),
    SelfType,
    Associated(String),
    MethodParameter(u32),
    /// Projection through a requirement's method-specific witness parameter.
    /// The witness index addresses [`RequirementDef::witness_params`].
    AssociatedProjection {
        witness: u32,
        associated: String,
    },
    View {
        mutable: bool,
        concept: ConceptId,
        bindings: BTreeMap<String, RequirementType>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConceptDef {
    pub id: ConceptId,
    pub name: String,
    pub span: Span,
    pub dynamic: bool,
    pub associated_types: Vec<AssociatedTypeDef>,
    /// Requirement ids in source declaration order.
    pub requirements: Vec<RequirementId>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssociatedTypeDef {
    pub name: String,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequirementDef {
    pub id: RequirementId,
    pub concept: ConceptId,
    pub name: String,
    pub span: Span,
    pub receiver: Option<Receiver>,
    pub method_type_parameters: u32,
    /// Includes the receiver as parameter zero for receiver requirements.
    pub params: Vec<RequirementType>,
    pub return_ty: RequirementType,
    /// Method-specific generic proof parameters, excluding conformance
    /// prerequisites supplied by a conditional witness application.
    pub witness_params: Vec<RequirementWitnessParam>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequirementWitnessParam {
    pub target: RequirementType,
    pub concept: ConceptId,
    pub bindings: BTreeMap<String, RequirementType>,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TypeDef {
    pub id: TypeId,
    pub name: String,
    pub span: Span,
    /// Declared generic arity, including phantom parameters not recoverable
    /// from field/payload shape.
    pub type_parameters: u32,
    pub kind: TypeDefKind,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TypeDefKind {
    Record {
        fields: Vec<FieldDef>,
        invariant: Option<Contract>,
    },
    Enum {
        variants: Vec<VariantDef>,
    },
    Refined {
        base: Type,
        predicate: Contract,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FieldDef {
    pub name: String,
    pub ty: Type,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VariantDef {
    pub id: VariantId,
    pub name: String,
    pub payload: Vec<Type>,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Function {
    pub id: FunctionId,
    pub name: String,
    pub span: Span,
    pub type_parameters: u32,
    #[serde(default)]
    pub is_async: bool,
    #[serde(default)]
    pub suspension_points: Vec<SuspensionPoint>,
    pub params: Vec<LocalDecl>,
    pub witness_params: Vec<WitnessParam>,
    /// Number of leading proof parameters owned by the enclosing
    /// conformance. The remaining suffix is owned by the implemented
    /// requirement. Non-witness functions always use zero.
    pub witness_prefix_count: u32,
    pub locals: Vec<LocalDecl>,
    pub return_ty: Type,
    pub receiver: Option<Receiver>,
    pub body: Block,
    pub call_plan: CallPlan,
}

impl Function {
    /// Iterates executable expressions in the canonical identity order.
    ///
    /// The iterator is stack-safe for deeply nested unchecked MIR and defines
    /// the preorder used by both identity assignment and validation.
    #[must_use]
    pub fn exprs_preorder(&self) -> ExprPreorder<'_> {
        ExprPreorder {
            pending: vec![ExprWalkNode::Block(&self.body)],
        }
    }

    /// Reassigns body expressions canonical, function-local dense ids.
    ///
    /// The traversal visits every expression before its children, preserves
    /// statement/argument/arm order, and visits a block tail after all of that
    /// block's statements.
    ///
    /// # Errors
    ///
    /// Returns an error if the function exhausts the usable [`ExprId`] range;
    /// [`u32::MAX`] remains permanently reserved for [`ExprId::UNASSIGNED`].
    pub fn renumber_expr_ids(&mut self) -> Result<(), ExprIdOverflow> {
        let mut assigner = ExprIdAssigner {
            function: self.id,
            next: 0,
        };
        assigner.assign_block(&mut self.body)
    }
}

/// A function contains more executable expressions than [`ExprId`] can name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExprIdOverflow {
    pub function: FunctionId,
}

impl fmt::Display for ExprIdOverflow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "function {} exhausts the usable expression-id domain",
            self.function.0
        )
    }
}

impl Error for ExprIdOverflow {}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SuspensionPoint {
    /// Resume states start at one; state zero is the coroutine entry.
    pub state: u32,
    pub span: Span,
    /// Locals whose current values may be read after resume or by a cleanup
    /// active at this suspension. Checked MIR requires this exact, sorted set.
    pub live_locals: Vec<LocalId>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalDecl {
    pub id: LocalId,
    pub name: String,
    pub ty: Type,
    pub mutable: bool,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Receiver {
    Readonly,
    Mutable,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CallPlan {
    pub receiver_invariant: Option<Contract>,
    pub requires: Vec<Contract>,
    pub ensures: Vec<Contract>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Contract {
    pub code: String,
    pub span: Span,
    pub expression: ContractExpr,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContractExpr {
    pub kind: ContractExprKind,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ContractExprKind {
    Constant(Constant),
    Value(ContractValue),
    /// Payload binding in the contract's lexical binding stack. The index is
    /// absolute: nested match arms retain outer slots and append their own.
    Binding(u32),
    Field(Box<ContractExpr>, u32),
    Unary(UnaryOp, Box<ContractExpr>),
    Binary(BinaryOp, Box<ContractExpr>, Box<ContractExpr>),
    IsFinite(Box<ContractExpr>),
    Match {
        scrutinee: Box<ContractExpr>,
        arms: Vec<ContractArm>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractValue {
    SelfValue,
    Result,
    Argument(u32),
    OldSelf,
    OldArgument(u32),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContractArm {
    pub pattern: Pattern,
    /// New binding slot types in pattern traversal order. These are appended
    /// to the lexical stack while validating/evaluating `value`.
    pub bindings: Vec<Type>,
    pub value: ContractExpr,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Block {
    pub statements: Vec<Statement>,
    pub tail: Option<Box<Expr>>,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Statement {
    pub kind: StatementKind,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum StatementKind {
    Let {
        local: LocalId,
        value: Expr,
    },
    /// Destructures a fixed tuple into locals in source order.
    LetTuple {
        locals: Vec<LocalId>,
        value: Expr,
    },
    /// Iterates `local` over the half-open integer range `[start, end)`.
    ForRange {
        local: LocalId,
        start: Box<Expr>,
        end: Box<Expr>,
        body: Box<Block>,
    },
    Assign {
        place: Place,
        value: Expr,
    },
    Assert {
        condition: Expr,
    },
    Evaluate(Expr),
    /// Registers a cleanup in the current lexical block. Backends execute
    /// registered blocks in LIFO order on every observable scope exit.
    Defer(Block),
    Return(Option<Expr>),
}

/// Checked-construction disposition fixed by semantic analysis.
///
/// `Plain` is valid only for records without an invariant. `Proven` carries a
/// process-local compiler proof and directly establishes the nominal value;
/// artifact decoding never trusts this wire spelling. `Recheck` preserves the
/// direct nominal shape while replaying a serialized construction's predicate
/// or invariant and raising `ArtifactProofRejected` if it no longer holds.
/// `Runtime` evaluates the predicate/invariant and returns `Result`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstructionMode {
    Plain,
    Proven,
    Recheck,
    Runtime,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Expr {
    /// Stable identity within the containing function. Checked MIR requires
    /// canonical preorder ids forming a dense range starting at zero.
    pub id: ExprId,
    pub kind: ExprKind,
    pub ty: Type,
    pub span: Span,
}

impl Expr {
    /// Constructs an unchecked expression for a later function-wide identity
    /// assignment pass.
    #[must_use]
    pub const fn new(kind: ExprKind, ty: Type, span: Span) -> Self {
        Self {
            id: ExprId::UNASSIGNED,
            kind,
            ty,
            span,
        }
    }
}

impl ExprId {
    /// Sentinel used only while constructing unchecked MIR. It cannot cross
    /// the checked MIR boundary because it is not a canonical preorder id.
    pub const UNASSIGNED: Self = Self(u32::MAX);
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ExprKind {
    Constant(Constant),
    Tuple(Vec<Expr>),
    List(Vec<Expr>),
    Copy(Place),
    Move(Place),
    Unary(UnaryOp, Box<Expr>),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    Block(Block),
    If {
        condition: Box<Expr>,
        then_branch: Block,
        else_branch: Block,
    },
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<MatchArm>,
    },
    Record {
        ty: TypeId,
        type_arguments: Vec<Type>,
        fields: Vec<Expr>,
        construction: ConstructionMode,
    },
    Variant {
        ty: TypeId,
        type_arguments: Vec<Type>,
        variant: VariantId,
        payload: Vec<Expr>,
    },
    Refine {
        ty: TypeId,
        value: Box<Expr>,
        construction: ConstructionMode,
    },
    /// Explicitly reads a constrained nominal value as its declared base.
    Unrefine(Box<Expr>),
    Call {
        target: CallTarget,
        /// Explicit instantiation shared by direct, inherent, and static
        /// concept calls. Dynamic and builtin calls require this to be empty.
        type_arguments: Vec<Type>,
        arguments: Vec<CallArgument>,
        witnesses: Vec<WitnessRef>,
    },
    MakeView {
        value: Box<Expr>,
        writeback: Option<Place>,
        witness: WitnessRef,
        mutable: bool,
        token: u32,
    },
    ReborrowView {
        owner: Place,
        mutable: bool,
        token: u32,
    },
    Await {
        /// State entered after the child task completes.
        state: u32,
        task: Box<Expr>,
    },
    /// Compiler-known timer task constructed from a nonnegative millisecond
    /// duration and consumed by `Await`.
    Sleep {
        milliseconds: Box<Expr>,
    },
    TaskJoin {
        mode: TaskJoinMode,
        arguments: Vec<Expr>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskJoinMode {
    All,
    Settled,
    Any,
    Race,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub bindings: Vec<LocalId>,
    pub value: Expr,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Pattern {
    Wildcard,
    Binding,
    Constant(Constant),
    Variant {
        ty: TypeId,
        variant: VariantId,
        payload: Vec<Pattern>,
    },
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Place {
    pub local: LocalId,
    pub projection: Vec<u32>,
}

impl Place {
    #[must_use]
    pub fn local(local: LocalId) -> Self {
        Self {
            local,
            projection: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum CallTarget {
    Direct(FunctionId),
    Inherent(FunctionId),
    StaticConcept {
        requirement: RequirementId,
        witness: WitnessRef,
        /// Resolved conformance `Self` at this call site. This remains explicit
        /// even for receiver requirements so static methods have the same ABI.
        dispatch_type: Type,
    },
    Dynamic {
        requirement: RequirementId,
    },
    Builtin(Builtin),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum CallArgument {
    Value(Expr),
    InOut(Place),
}

/// Stack-safe iterator over one function's expressions in canonical preorder.
pub struct ExprPreorder<'function> {
    pending: Vec<ExprWalkNode<'function>>,
}

enum ExprWalkNode<'function> {
    Block(&'function Block),
    Expr(&'function Expr),
}

impl<'function> ExprPreorder<'function> {
    fn push_statement(&mut self, statement: &'function Statement) {
        match &statement.kind {
            StatementKind::Let { value, .. }
            | StatementKind::LetTuple { value, .. }
            | StatementKind::Assign { value, .. } => {
                self.pending.push(ExprWalkNode::Expr(value));
            }
            StatementKind::ForRange {
                start, end, body, ..
            } => {
                self.pending.push(ExprWalkNode::Block(body));
                self.pending.push(ExprWalkNode::Expr(end));
                self.pending.push(ExprWalkNode::Expr(start));
            }
            StatementKind::Assert { condition } => {
                self.pending.push(ExprWalkNode::Expr(condition));
            }
            StatementKind::Evaluate(expression) => {
                self.pending.push(ExprWalkNode::Expr(expression));
            }
            StatementKind::Defer(cleanup) => {
                self.pending.push(ExprWalkNode::Block(cleanup));
            }
            StatementKind::Return(value) => {
                if let Some(value) = value {
                    self.pending.push(ExprWalkNode::Expr(value));
                }
            }
        }
    }

    fn push_children(&mut self, expression: &'function Expr) {
        match &expression.kind {
            ExprKind::Constant(_)
            | ExprKind::Copy(_)
            | ExprKind::Move(_)
            | ExprKind::ReborrowView { .. } => {}
            ExprKind::Tuple(elements)
            | ExprKind::List(elements)
            | ExprKind::Record {
                fields: elements, ..
            }
            | ExprKind::Variant {
                payload: elements, ..
            }
            | ExprKind::TaskJoin {
                arguments: elements,
                ..
            } => {
                for element in elements.iter().rev() {
                    self.pending.push(ExprWalkNode::Expr(element));
                }
            }
            ExprKind::Unary(_, operand)
            | ExprKind::Refine { value: operand, .. }
            | ExprKind::Unrefine(operand)
            | ExprKind::MakeView { value: operand, .. }
            | ExprKind::Await { task: operand, .. }
            | ExprKind::Sleep {
                milliseconds: operand,
            } => self.pending.push(ExprWalkNode::Expr(operand)),
            ExprKind::Binary(_, left, right) => {
                self.pending.push(ExprWalkNode::Expr(right));
                self.pending.push(ExprWalkNode::Expr(left));
            }
            ExprKind::Block(block) => self.pending.push(ExprWalkNode::Block(block)),
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.pending.push(ExprWalkNode::Block(else_branch));
                self.pending.push(ExprWalkNode::Block(then_branch));
                self.pending.push(ExprWalkNode::Expr(condition));
            }
            ExprKind::Match { scrutinee, arms } => {
                for arm in arms.iter().rev() {
                    self.pending.push(ExprWalkNode::Expr(&arm.value));
                }
                self.pending.push(ExprWalkNode::Expr(scrutinee));
            }
            ExprKind::Call { arguments, .. } => {
                for argument in arguments.iter().rev() {
                    if let CallArgument::Value(value) = argument {
                        self.pending.push(ExprWalkNode::Expr(value));
                    }
                }
            }
        }
    }
}

impl<'function> Iterator for ExprPreorder<'function> {
    type Item = &'function Expr;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.pending.pop()? {
                ExprWalkNode::Block(block) => {
                    if let Some(tail) = block.tail.as_deref() {
                        self.pending.push(ExprWalkNode::Expr(tail));
                    }
                    for statement in block.statements.iter().rev() {
                        self.push_statement(statement);
                    }
                }
                ExprWalkNode::Expr(expression) => {
                    self.push_children(expression);
                    return Some(expression);
                }
            }
        }
    }
}

struct ExprIdAssigner {
    function: FunctionId,
    next: u64,
}

impl ExprIdAssigner {
    fn allocate(&mut self) -> Result<ExprId, ExprIdOverflow> {
        if self.next >= u64::from(ExprId::UNASSIGNED.0) {
            return Err(ExprIdOverflow {
                function: self.function,
            });
        }
        let raw = u32::try_from(self.next).map_err(|_| ExprIdOverflow {
            function: self.function,
        })?;
        self.next += 1;
        Ok(ExprId(raw))
    }

    fn assign_block(&mut self, block: &mut Block) -> Result<(), ExprIdOverflow> {
        for statement in &mut block.statements {
            self.assign_statement(statement)?;
        }
        if let Some(tail) = &mut block.tail {
            self.assign_expr(tail)?;
        }
        Ok(())
    }

    fn assign_statement(&mut self, statement: &mut Statement) -> Result<(), ExprIdOverflow> {
        match &mut statement.kind {
            StatementKind::Let { value, .. }
            | StatementKind::LetTuple { value, .. }
            | StatementKind::Assign { value, .. } => self.assign_expr(value),
            StatementKind::ForRange {
                start, end, body, ..
            } => {
                self.assign_expr(start)?;
                self.assign_expr(end)?;
                self.assign_block(body)
            }
            StatementKind::Assert { condition } => self.assign_expr(condition),
            StatementKind::Evaluate(expression) => self.assign_expr(expression),
            StatementKind::Defer(cleanup) => self.assign_block(cleanup),
            StatementKind::Return(value) => {
                if let Some(value) = value {
                    self.assign_expr(value)?;
                }
                Ok(())
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn assign_expr(&mut self, expression: &mut Expr) -> Result<(), ExprIdOverflow> {
        expression.id = self.allocate()?;
        match &mut expression.kind {
            ExprKind::Constant(_)
            | ExprKind::Copy(_)
            | ExprKind::Move(_)
            | ExprKind::ReborrowView { .. } => Ok(()),
            ExprKind::Tuple(elements)
            | ExprKind::List(elements)
            | ExprKind::Record {
                fields: elements, ..
            }
            | ExprKind::Variant {
                payload: elements, ..
            }
            | ExprKind::TaskJoin {
                arguments: elements,
                ..
            } => {
                for element in elements {
                    self.assign_expr(element)?;
                }
                Ok(())
            }
            ExprKind::Unary(_, operand)
            | ExprKind::Refine { value: operand, .. }
            | ExprKind::Unrefine(operand)
            | ExprKind::MakeView { value: operand, .. }
            | ExprKind::Await { task: operand, .. }
            | ExprKind::Sleep {
                milliseconds: operand,
            } => self.assign_expr(operand),
            ExprKind::Binary(_, left, right) => {
                self.assign_expr(left)?;
                self.assign_expr(right)
            }
            ExprKind::Block(block) => self.assign_block(block),
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.assign_expr(condition)?;
                self.assign_block(then_branch)?;
                self.assign_block(else_branch)
            }
            ExprKind::Match { scrutinee, arms } => {
                self.assign_expr(scrutinee)?;
                for arm in arms {
                    self.assign_expr(&mut arm.value)?;
                }
                Ok(())
            }
            ExprKind::Call { arguments, .. } => {
                for argument in arguments {
                    if let CallArgument::Value(value) = argument {
                        self.assign_expr(value)?;
                    }
                }
                Ok(())
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Builtin {
    IsFinite,
    ParseFloat,
    FormatFloat,
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
    ListAdd,
    ListLength,
    ListGet,
    ProcessArguments,
    ProcessEnvironment,
    ParseInt,
    TaskFaultCode,
    TaskFaultMessage,
    DurationMilliseconds,
    DurationAsMilliseconds,
    FileOpenRead,
    FileCreate,
    FileOpenReadPath,
    FileCreatePath,
    FileReadText,
    FileWriteText,
    FileClose,
    SocketConnect,
    SocketReadText,
    SocketWriteText,
    SocketClose,
    TextMapNew,
    TextMapLength,
    TextMapContains,
    TextMapGet,
    TextMapInsert,
    TextMapRemove,
    JsonParse,
    JsonFormat,
    IoErrorKind,
    IoErrorMessage,
    FileTryOpenRead,
    FileTryCreate,
    FileTryOpenReadPath,
    FileTryCreatePath,
    FileTryReadText,
    FileTryWriteText,
    SocketTryConnect,
    SocketTryReadText,
    SocketTryWriteText,
    LogDebug,
    LogInfo,
    LogWarn,
    LogError,
    LogWrite,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Witness {
    pub id: WitnessId,
    pub concept: ConceptId,
    pub concrete: Type,
    pub methods: BTreeMap<RequirementId, FunctionId>,
    pub associated: BTreeMap<String, Type>,
    /// Number of alpha-normalized parameters used by `concrete`, associated
    /// bindings, and prerequisite schemas.
    pub type_parameters: u32,
    /// Proof schema for a conditional conformance. Concrete proof trees live
    /// at call sites in [`WitnessRef::Apply`].
    pub prerequisites: Vec<WitnessParam>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WitnessParam {
    pub target: Type,
    pub concept: ConceptId,
    pub bindings: BTreeMap<String, Type>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WitnessRef {
    Concrete(WitnessId),
    Parameter(u32),
    Apply {
        witness: WitnessId,
        arguments: Vec<WitnessRef>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Constant {
    Unit,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnaryOp {
    Negate,
    Not,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BinaryOp {
    Add,
    Subtract,
    Multiply,
    Divide,
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    And,
    Or,
}
