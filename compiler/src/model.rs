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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Type {
    Int,
    Bool,
    Unit,
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

pub mod ast {
    use super::{Binary, Span, Unary};

    #[derive(Clone, Debug, Default)]
    pub struct File {
        pub imports: Vec<Import>,
        pub functions: Vec<Function>,
    }

    #[derive(Clone, Debug)]
    pub struct Import {
        pub path: Vec<String>,
        pub span: Span,
    }

    #[derive(Clone, Debug)]
    pub struct Function {
        pub name: String,
        pub public: bool,
        pub test: bool,
        pub params: Vec<Param>,
        pub result: Option<String>,
        pub requires: Vec<Expr>,
        pub ensures: Vec<Expr>,
        pub body: Block,
        pub span: Span,
    }

    #[derive(Clone, Debug)]
    pub struct Param {
        pub name: String,
        pub ty: String,
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
            annotation: Option<String>,
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
        Name(Vec<String>),
        Unary(Unary, Box<Expr>),
        Binary(Binary, Box<Expr>, Box<Expr>),
        Call(Vec<String>, Vec<Expr>),
        If {
            condition: Box<Expr>,
            then_body: Block,
            else_body: Option<Block>,
        },
    }
}

#[derive(Clone, Debug)]
pub struct PackageFile {
    pub package: String,
    /// Helpers in *_test.loom are unavailable to production declarations.
    pub test_only: bool,
    pub syntax: ast::File,
}

pub mod checked {
    use super::{Binary, Span, Type, Unary};

    #[derive(Debug)]
    pub struct Program {
        pub functions: Vec<Function>,
        pub entry: Option<usize>,
        pub tests: Vec<usize>,
        /// Public functions of the selected package, for a library object.
        pub exports: Vec<usize>,
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
        Local(usize),
        Unary(Unary, Box<Expr>),
        Binary(Binary, Box<Expr>, Box<Expr>),
        Call(usize, Vec<Expr>),
        If {
            condition: Box<Expr>,
            then_body: Block,
            else_body: Option<Block>,
        },
    }
}
