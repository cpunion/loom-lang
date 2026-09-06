//! Source and checked forms for the native seed. No serialized intermediate ABI.

use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub source: usize,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub span: Span,
    pub message: String,
}

impl Diagnostic {
    pub fn new(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
        }
    }
}

#[derive(Debug)]
pub struct Source {
    pub path: PathBuf,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Type {
    Int,
    Bool,
    Text,
    Bytes,
    /// Interned element type in Program::lists.
    List(usize),
    Unit,
    Data(usize),
    /// Used only to check a generic definition, never in emitted instances.
    Parameter(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unary {
    Neg,
    Not,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binary {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

/// Irreducible private standard-library operations, not public API dispatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Primitive {
    TextLen,
    TextByte,
    TextConcat,
    TextEqual,
    BytesNew,
    BytesLen,
    BytesPush,
    BytesUtf8,
    BytesTextCopy,
    ListNew,
    ListLen,
    ListGet,
    ListPush,
    ListSet,
    Open,
    Read,
    Close,
}

pub mod ast {
    use super::{Binary, Span, Unary};

    #[derive(Clone, Debug, Default)]
    pub struct File {
        pub imports: Vec<Import>,
        pub functions: Vec<Function>,
        pub data: Vec<Data>,
    }

    #[derive(Clone, Debug)]
    pub struct TypeRef {
        pub path: Vec<String>,
        pub args: Vec<TypeRef>,
        pub span: Span,
    }

    #[derive(Clone, Debug)]
    pub struct Data {
        pub name: String,
        pub public: bool,
        pub parameters: Vec<String>,
        pub kind: DataKind,
        pub span: Span,
    }

    #[derive(Clone, Debug)]
    pub enum DataKind {
        Record(Vec<Field>),
        Enum(Vec<Variant>),
    }

    #[derive(Clone, Debug)]
    pub struct Field {
        pub name: String,
        pub ty: TypeRef,
        pub span: Span,
    }

    #[derive(Clone, Debug)]
    pub struct Variant {
        pub name: String,
        pub fields: Vec<TypeRef>,
        pub span: Span,
    }

    #[derive(Clone, Debug)]
    pub struct Import {
        pub path: Vec<String>,
        pub span: Span,
    }

    #[derive(Clone, Debug)]
    pub struct Function {
        pub name: String,
        pub intrinsic: bool,
        pub public: bool,
        pub test: bool,
        pub parameters: Vec<String>,
        pub params: Vec<Param>,
        pub result: Option<TypeRef>,
        pub requires: Vec<Expr>,
        pub ensures: Vec<Expr>,
        pub body: Block,
        pub span: Span,
    }

    #[derive(Clone, Debug)]
    pub struct Param {
        pub name: String,
        pub ty: TypeRef,
        pub span: Span,
    }

    pub type Block = Vec<Stmt>;

    #[derive(Clone, Debug)]
    pub struct Stmt {
        pub kind: StmtKind,
        pub span: Span,
    }

    #[derive(Clone, Debug)]
    pub enum StmtKind {
        Let {
            name: String,
            mutable: bool,
            annotation: Option<TypeRef>,
            value: Expr,
        },
        Assign {
            name: String,
            value: Expr,
        },
        Return(Option<Expr>),
        Assert(Expr),
        Discard(Expr),
        Expr(Expr),
        While {
            condition: Expr,
            body: Block,
        },
    }

    #[derive(Clone, Debug)]
    pub struct Expr {
        pub kind: ExprKind,
        pub span: Span,
    }

    #[derive(Clone, Debug)]
    pub enum ExprKind {
        Int(i64),
        Bool(bool),
        Text(String),
        Name(Vec<String>),
        Unary(Unary, Box<Expr>),
        Binary(Binary, Box<Expr>, Box<Expr>),
        Call {
            path: Vec<String>,
            types: Vec<TypeRef>,
            args: Vec<Expr>,
        },
        Record {
            ty: TypeRef,
            fields: Vec<(String, Expr)>,
        },
        Field(Box<Expr>, String),
        Match {
            value: Box<Expr>,
            arms: Vec<MatchArm>,
        },
        Block(Block),
        If {
            condition: Box<Expr>,
            then_body: Block,
            else_body: Option<Block>,
        },
    }

    #[derive(Clone, Debug)]
    pub struct MatchArm {
        pub pattern: Pattern,
        pub body: Block,
        pub span: Span,
    }

    #[derive(Clone, Debug)]
    pub enum Pattern {
        Wildcard,
        Name(Vec<String>),
        Variant {
            path: Vec<String>,
            bindings: Vec<Option<String>>,
        },
    }
}

#[derive(Clone, Debug)]
pub struct PackageFile {
    pub package: String,
    /// Source was resolved from the configured compiler standard-library root.
    pub trusted_std: bool,
    /// Helpers in *_test.loom are unavailable to production declarations.
    pub test_only: bool,
    pub syntax: ast::File,
}

pub mod checked {
    use super::{Binary, Primitive, Span, Type, Unary};

    #[derive(Debug)]
    pub struct Program {
        pub types: Vec<Data>,
        pub lists: Vec<Type>,
        pub functions: Vec<Function>,
        pub entry: Option<usize>,
        pub tests: Vec<usize>,
        /// Public functions of the selected package, for a library object.
        pub exports: Vec<usize>,
    }

    #[derive(Clone, Debug)]
    pub struct Data {
        pub name: String,
        pub kind: DataKind,
    }

    #[derive(Clone, Debug)]
    pub enum DataKind {
        Record(Vec<(String, Type)>),
        Enum(Vec<(String, Vec<Type>)>),
    }

    #[derive(Debug)]
    pub struct Function {
        pub name: String,
        pub params: Vec<Type>,
        pub result: Type,
        /// Parameters occupy the first slots. Every local has one storage type.
        pub locals: Vec<Type>,
        pub requires: Vec<Expr>,
        pub body: Block,
        pub span: Span,
    }

    #[derive(Clone, Debug)]
    pub struct Block {
        pub statements: Vec<Stmt>,
        pub tail: Option<Box<Expr>>,
        /// True if execution can reach the end without returning from the function.
        pub falls_through: bool,
    }

    #[derive(Clone, Debug)]
    pub struct Stmt {
        pub kind: StmtKind,
        pub span: Span,
    }

    #[derive(Clone, Debug)]
    pub enum StmtKind {
        Let { local: usize, value: Expr },
        Assign { local: usize, value: Expr },
        Return(Option<Expr>),
        Assert(Expr),
        Discard(Expr),
        Expr(Expr),
        While { condition: Expr, body: Block },
    }

    #[derive(Clone, Debug)]
    pub struct Expr {
        pub kind: ExprKind,
        pub ty: Type,
        pub span: Span,
    }

    #[derive(Clone, Debug)]
    pub enum ExprKind {
        Int(i64),
        Bool(bool),
        Text(String),
        Primitive(Primitive, Vec<Expr>),
        Local(usize),
        Unary(Unary, Box<Expr>),
        Binary(Binary, Box<Expr>, Box<Expr>),
        Call(usize, Vec<Expr>),
        Record(Vec<(usize, Expr)>),
        Field(Box<Expr>, usize),
        Variant {
            variant: usize,
            fields: Vec<Expr>,
        },
        Match {
            value: Box<Expr>,
            arms: Vec<MatchArm>,
        },
        Block(Block),
        If {
            condition: Box<Expr>,
            then_body: Block,
            else_body: Option<Block>,
        },
    }

    #[derive(Clone, Debug)]
    pub struct MatchArm {
        /// None is an irrefutable arm; otherwise the selected enum variant.
        pub variant: Option<usize>,
        pub bindings: Vec<Option<usize>>,
        /// A binding of the whole scrutinee for a bare-name pattern.
        pub whole: Option<usize>,
        pub body: Block,
    }
}
