//! Checked native IR and its minimal layout and source-location descriptors.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Type {
    Int,
    Float,
    Bool,
    Text,
    Bytes,
    /// Interned element type in Program::lists.
    List(usize),
    /// Nominal erased interface in Program::interfaces.
    Dyn(usize),
    /// Structural signature in Program::function_types; an unmanaged code pointer.
    Function(usize),
    Unit,
    Data(usize),
    /// Describes an unused generic type template; invalid in emitted instances.
    Parameter(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unary {
    Neg,
    Not,
    BitNot,
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
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

/// Irreducible private standard-library operations, not public API dispatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Primitive {
    FloatFromInt,
    FloatToInt,
    FloatParse,
    FloatFormat,
    TextLen,
    TextByte,
    TextConcat,
    TextEqual,
    TextSlice,
    UnicodeAlphabetic,
    UnicodeAlphanumeric,
    UnicodeWhitespace,
    ArgCount,
    ArgText,
    ProcessRun,
    ProcessRunInput,
    Exit,
    BytesNew,
    BytesLen,
    BytesGet,
    BytesPush,
    BytesSet,
    BytesUtf8,
    BytesTextCopy,
    ListNew,
    ListLen,
    ListGet,
    ListPush,
    ListSet,
    Open,
    Create,
    Read,
    Write,
    WriteBytes,
    Close,
    DirectoryRead,
    PathKind,
    PathCanonical,
}

pub mod checked {
    use super::{Binary, Primitive, Span, Type, Unary};

    #[derive(Debug)]
    pub struct Program {
        pub types: Vec<Data>,
        pub lists: Vec<Type>,
        pub functions: Vec<Function>,
        pub function_types: Vec<Signature>,
        pub interfaces: Vec<Interface>,
        pub witnesses: Vec<Witness>,
        pub entry: Option<usize>,
        pub tests: Vec<usize>,
        /// Public functions of the selected package, for a library object.
        pub exports: Vec<usize>,
    }

    #[derive(Debug)]
    pub struct Signature {
        pub params: Vec<Type>,
        pub result: Type,
    }

    #[derive(Debug)]
    pub struct Interface {
        /// Method signatures exclude the erased receiver.
        pub methods: Vec<Signature>,
    }

    #[derive(Debug)]
    pub struct Witness {
        pub interface: usize,
        pub concrete: Type,
        /// Absent slots are unreferenced by the selected closed-world program.
        pub methods: Vec<Option<usize>>,
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
        Refined(Type),
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
        Float(f64),
        Bool(bool),
        Text(String),
        Primitive(Primitive, Vec<Expr>),
        /// Same native representation; construction or safe scalar weakening.
        Coerce(Box<Expr>),
        Local(usize),
        Unary(Unary, Box<Expr>),
        Binary(Binary, Box<Expr>, Box<Expr>),
        Call(usize, Vec<Expr>),
        FunctionRef(usize),
        IndirectCall {
            callee: Box<Expr>,
            arguments: Vec<Expr>,
        },
        DynBox {
            witness: usize,
            value: Box<Expr>,
        },
        DynCall {
            receiver: Box<Expr>,
            slot: usize,
            arguments: Vec<Expr>,
        },
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
