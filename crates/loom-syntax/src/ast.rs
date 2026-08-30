//! Source-shaped abstract syntax tree populated by the recovering parser.
//!
//! The AST deliberately keeps source spellings and ranges rather than resolved
//! names or typed values. The complete lossless token stream lives alongside it
//! in [`crate::Parse`].

use loom_core::TextRange;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceFile {
    pub imports: Vec<ImportDecl>,
    pub declarations: Vec<Decl>,
    pub range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImportDecl {
    pub path: Path,
    pub range: TextRange,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum Visibility {
    #[default]
    Private,
    Public,
}

impl Visibility {
    #[must_use]
    pub const fn is_public(self) -> bool {
        matches!(self, Self::Public)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Decl {
    pub visibility: Visibility,
    pub kind: DeclKind,
    pub range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DeclKind {
    Constant(ConstantDecl),
    ConstrainedType(ConstrainedTypeDecl),
    Record(RecordDecl),
    Enum(EnumDecl),
    Function(FunctionDecl),
    Impl(ImplDecl),
    Concept(ConceptDecl),
    Error(ErrorNode),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConstantDecl {
    pub name: Ident,
    pub ty: TypeExpr,
    pub value: Expr,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConstrainedTypeDecl {
    pub name: Ident,
    pub base: TypeExpr,
    pub predicate: Expr,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RecordDecl {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub fields: Vec<RecordField>,
    pub invariant: Option<Expr>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RecordField {
    pub name: Ident,
    pub ty: TypeExpr,
    pub range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EnumDecl {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub variants: Vec<EnumVariant>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EnumVariant {
    pub name: Ident,
    pub payload: Vec<TypeExpr>,
    pub range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FunctionDecl {
    pub signature: CallableSignature,
    pub body: Block,
    pub is_test: bool,
    pub is_async: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CallableSignature {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub receiver: Option<Receiver>,
    pub parameters: Vec<Parameter>,
    /// `None` is the source spelling for a fixed implicit `Unit` return.
    /// Later phases must not infer a return type from the body in this case.
    pub return_type: Option<TypeExpr>,
    pub contracts: Vec<Contract>,
    pub range: TextRange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Receiver {
    ReadOnly(TextRange),
    Mutable(TextRange),
}

impl Receiver {
    #[must_use]
    pub const fn range(self) -> TextRange {
        match self {
            Self::ReadOnly(range) | Self::Mutable(range) => range,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Parameter {
    pub name: Ident,
    pub ty: TypeExpr,
    pub range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GenericParam {
    pub name: Ident,
    pub bounds: Vec<ConceptRef>,
    pub range: TextRange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ContractKind {
    Requires,
    Ensures,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Contract {
    pub kind: ContractKind,
    pub predicate: Expr,
    pub range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImplDecl {
    pub generics: Vec<GenericParam>,
    pub kind: ImplKind,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ImplKind {
    Inherent {
        target: TypeExpr,
        methods: Vec<MethodDecl>,
    },
    Conformance {
        concept: ConceptRef,
        target: TypeExpr,
        members: Vec<ConformanceMember>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MethodDecl {
    pub visibility: Visibility,
    pub is_static: bool,
    pub signature: CallableSignature,
    pub body: Block,
    pub range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ConformanceMember {
    AssociatedType(AssociatedTypeBinding),
    Method(MethodDecl),
    Error(ErrorNode),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AssociatedTypeBinding {
    pub name: Ident,
    pub value: TypeExpr,
    pub range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConceptDecl {
    pub name: Ident,
    pub dynamic: bool,
    pub members: Vec<ConceptMember>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ConceptMember {
    AssociatedType(AssociatedTypeRequirement),
    Method(MethodRequirement),
    Error(ErrorNode),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AssociatedTypeRequirement {
    pub name: Ident,
    pub bounds: Vec<ConceptRef>,
    pub range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MethodRequirement {
    pub is_static: bool,
    pub signature: CallableSignature,
    pub range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Ident {
    pub text: String,
    pub range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Path {
    pub segments: Vec<Ident>,
    pub range: TextRange,
}

impl Path {
    #[must_use]
    pub fn as_string(&self) -> String {
        self.segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<Vec<_>>()
            .join(".")
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConceptRef {
    pub path: Path,
    pub bindings: Vec<AssociatedBinding>,
    pub range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AssociatedBinding {
    pub name: Ident,
    pub ty: TypeExpr,
    pub range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TypeExpr {
    pub kind: TypeExprKind,
    pub range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TypeExprKind {
    /// A fixed-size structural product type. `(T)` remains a grouped type;
    /// tuples require a comma, including the one-element form `(T,)`.
    Tuple(Vec<TypeExpr>),
    Named {
        path: Path,
        arguments: Vec<TypeArgument>,
    },
    QualifiedProjection {
        base: Box<TypeExpr>,
        concept: ConceptRef,
        associated: Ident,
    },
    /// A dynamically dispatched concept value type written as `dyn C`.
    BareDyn(ConceptRef),
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TypeArgument {
    Type(TypeExpr),
    Binding(AssociatedBinding),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Expr {
    pub kind: ExprKind,
    pub range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ExprKind {
    Literal(Literal),
    /// A fixed-size structural product value. Parentheses without a comma are
    /// grouping rather than tuple construction.
    Tuple(Vec<Expr>),
    /// A homogeneous, dynamically sized list value. Empty literals require an
    /// expected `List[T]` type from their context.
    List(Vec<Expr>),
    Name(Path),
    SelfValue,
    ContractResult,
    Old(Box<Expr>),
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Member {
        receiver: Box<Expr>,
        name: Ident,
    },
    QualifiedMember {
        base: TypeExpr,
        concept: ConceptRef,
        name: Ident,
    },
    Call {
        callee: Box<Expr>,
        type_arguments: Vec<TypeExpr>,
        arguments: Vec<Expr>,
    },
    Await(Box<Expr>),
    /// Propagates the `Err` branch of a `Result` from the current callable.
    Propagate(Box<Expr>),
    RecordLiteral {
        constructor: Path,
        fields: Vec<RecordLiteralField>,
    },
    If {
        condition: Box<Expr>,
        then_branch: Block,
        else_branch: Option<ElseBranch>,
    },
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<MatchArm>,
    },
    Block(Block),
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ElseBranch {
    Block(Block),
    If(Box<Expr>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum UnaryOp {
    Negate,
    Not,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum BinaryOp {
    Multiply,
    Divide,
    Add,
    Subtract,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Equal,
    NotEqual,
    And,
    Or,
}

impl BinaryOp {
    #[must_use]
    pub const fn is_comparison(self) -> bool {
        matches!(
            self,
            Self::Less
                | Self::LessEqual
                | Self::Greater
                | Self::GreaterEqual
                | Self::Equal
                | Self::NotEqual
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Literal {
    Int(String),
    Float(String),
    Text(String),
    Bool(bool),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RecordLiteralField {
    pub name: Ident,
    pub value: Expr,
    pub range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Block {
    pub items: Vec<BlockItem>,
    pub range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum BlockItem {
    Local(LocalBinding),
    ForRange(ForRange),
    While(While),
    Break(TextRange),
    Continue(TextRange),
    Defer(Block),
    Discard(Expr),
    Return(ReturnExpr),
    Assert(Expr),
    Assignment(Assignment),
    Expr(Expr),
    Error(ErrorNode),
}

/// A half-open integer range loop: `for name in start..end { ... }`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ForRange {
    pub binding: Ident,
    pub start: Expr,
    pub end: Expr,
    pub body: Block,
    pub range: TextRange,
}

/// A condition-controlled loop: `while condition { ... }`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct While {
    pub condition: Expr,
    pub body: Block,
    pub range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LocalBinding {
    pub mutable: bool,
    pub scoped: bool,
    /// One name for an ordinary binding, or multiple names for tuple
    /// destructuring (`let a, b = value`).
    pub names: Vec<Ident>,
    pub annotation: Option<TypeExpr>,
    pub value: Expr,
    pub range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReturnExpr {
    pub value: Option<Expr>,
    pub range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Assignment {
    pub target: Expr,
    pub value: Expr,
    pub range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub value: Expr,
    pub range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Pattern {
    pub kind: PatternKind,
    pub range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PatternKind {
    Wildcard,
    Literal(Literal),
    /// An unresolved name-pattern. Resolution decides whether a bare,
    /// payload-free name is a binding or a nullary constructor. Qualified and
    /// payload-bearing names can only resolve as constructors.
    Name {
        path: Path,
        payload: Vec<Pattern>,
    },
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ErrorNode {
    pub range: TextRange,
}
