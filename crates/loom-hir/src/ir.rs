//! Source-independent declaration and body representation.

use std::collections::BTreeMap;

use loom_core::{FileId, ModuleName, Name, PackageId, Span};
use serde::{Deserialize, Serialize};

use crate::{
    Arena, BodyId, BodySourceMap, DefId, ExprId, GenericParamId, LocalId, ModuleId, ParamId,
    PatternId, ProgramSourceMap, TypeRefId,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Visibility {
    Private,
    Public,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathSegment {
    pub name: Name,
    pub span: Span,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Path {
    pub segments: Vec<PathSegment>,
}

impl Path {
    #[must_use]
    pub fn from_name(name: Name, span: Span) -> Self {
        Self {
            segments: vec![PathSegment { name, span }],
        }
    }

    #[must_use]
    pub fn last(&self) -> Option<&Name> {
        self.segments.last().map(|segment| &segment.name)
    }

    #[must_use]
    pub fn as_string(&self) -> String {
        self.segments
            .iter()
            .map(|segment| segment.name.as_str())
            .collect::<Vec<_>>()
            .join(".")
    }
}

#[derive(Clone, Debug)]
pub struct Import {
    pub path: Path,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Module {
    pub package: PackageId,
    pub name: ModuleName,
    pub files: Vec<FileId>,
    pub imports: Vec<Import>,
    pub items: Vec<DefId>,
}

#[derive(Clone, Debug)]
pub struct Definition {
    pub module: ModuleId,
    pub name: Option<Name>,
    pub visibility: Visibility,
    pub kind: DefinitionKind,
}

#[derive(Clone, Debug)]
pub enum DefinitionKind {
    Error,
    RefinedType(RefinedTypeDef),
    Record(RecordDef),
    Field(FieldDef),
    Enum(EnumDef),
    Variant(VariantDef),
    Function(FunctionDef),
    Test(FunctionDef),
    InherentImpl(ImplDef),
    Concept(ConceptDef),
    AssociatedType(AssociatedTypeDef),
    Conformance(ConformanceDef),
    Method(MethodDef),
}

impl DefinitionKind {
    #[must_use]
    pub const fn tag(&self) -> DefinitionTag {
        match self {
            Self::Error => DefinitionTag::Error,
            Self::RefinedType(_) => DefinitionTag::RefinedType,
            Self::Record(_) => DefinitionTag::Record,
            Self::Field(_) => DefinitionTag::Field,
            Self::Enum(_) => DefinitionTag::Enum,
            Self::Variant(_) => DefinitionTag::Variant,
            Self::Function(_) => DefinitionTag::Function,
            Self::Test(_) => DefinitionTag::Test,
            Self::InherentImpl(_) => DefinitionTag::InherentImpl,
            Self::Concept(_) => DefinitionTag::Concept,
            Self::AssociatedType(_) => DefinitionTag::AssociatedType,
            Self::Conformance(_) => DefinitionTag::Conformance,
            Self::Method(_) => DefinitionTag::Method,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DefinitionTag {
    Error,
    RefinedType,
    Record,
    Field,
    Enum,
    Variant,
    Function,
    Test,
    InherentImpl,
    Concept,
    AssociatedType,
    Conformance,
    Method,
}

#[derive(Clone, Debug)]
pub struct RefinedTypeDef {
    pub base: TypeRefId,
    pub predicate: BodyId,
}

#[derive(Clone, Debug, Default)]
pub struct RecordDef {
    pub generic_params: Vec<GenericParamId>,
    pub fields: Vec<DefId>,
    pub invariant: Option<BodyId>,
}

#[derive(Clone, Debug)]
pub struct FieldDef {
    pub owner: DefId,
    pub ty: TypeRefId,
}

#[derive(Clone, Debug, Default)]
pub struct EnumDef {
    pub generic_params: Vec<GenericParamId>,
    pub variants: Vec<DefId>,
}

#[derive(Clone, Debug)]
pub struct VariantDef {
    pub owner: DefId,
    pub payload: Vec<TypeRefId>,
}

#[derive(Clone, Debug)]
pub struct FunctionDef {
    pub signature: CallableSignature,
    pub body: BodyId,
    pub is_async: bool,
}

#[derive(Clone, Debug)]
pub struct ImplDef {
    pub generic_params: Vec<GenericParamId>,
    pub target: TypeRefId,
    pub methods: Vec<DefId>,
}

#[derive(Clone, Debug, Default)]
pub struct ConceptDef {
    pub dyn_capable: bool,
    pub associated_types: Vec<DefId>,
    pub requirements: Vec<DefId>,
}

#[derive(Clone, Debug)]
pub struct AssociatedTypeDef {
    pub owner: DefId,
    pub bounds: Vec<ConceptRef>,
    pub binding: Option<TypeRefId>,
}

#[derive(Clone, Debug)]
pub struct ConformanceDef {
    pub generic_params: Vec<GenericParamId>,
    pub concept: ConceptRef,
    pub target: TypeRefId,
    pub associated_types: Vec<DefId>,
    pub methods: Vec<DefId>,
}

#[derive(Clone, Debug)]
pub struct MethodDef {
    pub owner: DefId,
    pub signature: CallableSignature,
    pub body: Option<BodyId>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ReceiverKind {
    ReadOnly,
    Mutable,
    Static,
}

#[derive(Clone, Debug, Default)]
pub struct CallableSignature {
    pub generic_params: Vec<GenericParamId>,
    pub receiver: Option<ReceiverKind>,
    pub params: Vec<ParamId>,
    /// `None` denotes the source-level omitted return, which semantically is
    /// always `Unit`; it is never a request for return-type inference.
    pub return_ty: Option<TypeRefId>,
    pub contracts: Contracts,
}

#[derive(Clone, Debug)]
pub struct GenericParam {
    pub owner: DefId,
    pub name: Name,
    pub bounds: Vec<ConceptRef>,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub owner: DefId,
    pub name: Name,
    pub ty: TypeRefId,
}

#[derive(Clone, Debug, Default)]
pub struct Contracts {
    pub requires: Vec<BodyId>,
    pub ensures: Vec<BodyId>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConceptRef {
    pub path: Path,
    pub bindings: Vec<AssociatedBindingRef>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssociatedBindingRef {
    pub name: Name,
    pub ty: TypeRefId,
}

#[derive(Clone, Debug)]
pub enum TypeRef {
    Error,
    Tuple(Vec<TypeRefId>),
    Path(Path),
    Apply {
        constructor: Path,
        arguments: Vec<TypeArgumentRef>,
    },
    SelfType,
    Projection {
        self_ty: TypeRefId,
        concept: Option<Path>,
        associated: Name,
    },
    Dyn(ConceptRef),
}

#[derive(Clone, Debug)]
pub enum TypeArgumentRef {
    Type(TypeRefId),
    Binding(AssociatedBindingRef),
}

#[derive(Clone, Debug)]
pub struct Body {
    pub owner: DefId,
    pub kind: BodyKind,
    pub locals: Arena<LocalId, Local>,
    pub expressions: Arena<ExprId, Expr>,
    pub patterns: Arena<PatternId, Pattern>,
    pub root: ExprId,
    pub source_map: BodySourceMap,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BodyKind {
    Function,
    Method,
    RefinementPredicate,
    RecordInvariant,
    Requires,
    Ensures,
}

/// Incrementally constructs a body while keeping its source maps aligned.
#[derive(Clone, Debug)]
pub struct BodyBuilder {
    owner: DefId,
    kind: BodyKind,
    locals: Arena<LocalId, Local>,
    expressions: Arena<ExprId, Expr>,
    patterns: Arena<PatternId, Pattern>,
    source_map: BodySourceMap,
}

impl BodyBuilder {
    #[must_use]
    pub fn new(owner: DefId, kind: BodyKind) -> Self {
        Self {
            owner,
            kind,
            locals: Arena::default(),
            expressions: Arena::default(),
            patterns: Arena::default(),
            source_map: BodySourceMap::default(),
        }
    }

    pub fn alloc_local(&mut self, local: Local, span: Span) -> LocalId {
        let id = self.locals.alloc(local);
        self.source_map.insert_local(id, span);
        id
    }

    pub fn alloc_expr(&mut self, expression: Expr, span: Span) -> ExprId {
        let id = self.expressions.alloc(expression);
        self.source_map.insert_expr(id, span);
        id
    }

    pub fn alloc_pattern(&mut self, pattern: Pattern, span: Span) -> PatternId {
        let id = self.patterns.alloc(pattern);
        self.source_map.insert_pattern(id, span);
        id
    }

    #[must_use]
    pub fn finish(self, root: ExprId) -> Body {
        Body {
            owner: self.owner,
            kind: self.kind,
            locals: self.locals,
            expressions: self.expressions,
            patterns: self.patterns,
            root,
            source_map: self.source_map,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Local {
    pub name: Name,
    pub mutable: bool,
    pub annotation: Option<TypeRefId>,
}

#[derive(Clone, Debug)]
pub enum Expr {
    Error,
    Literal(Literal),
    Tuple(Vec<ExprId>),
    List(Vec<ExprId>),
    Path(Path),
    SelfValue,
    ResultValue,
    Old(ExprId),
    Block {
        statements: Vec<Statement>,
        tail: Option<ExprId>,
    },
    If {
        condition: ExprId,
        then_branch: ExprId,
        else_branch: Option<ExprId>,
    },
    Match {
        scrutinee: ExprId,
        arms: Vec<MatchArm>,
    },
    Call {
        callee: ExprId,
        type_arguments: Vec<TypeRefId>,
        arguments: Vec<ExprId>,
    },
    MethodCall {
        receiver: ExprId,
        method: Name,
        type_arguments: Vec<TypeRefId>,
        arguments: Vec<ExprId>,
    },
    QualifiedMethodCall {
        self_ty: TypeRefId,
        concept: ConceptRef,
        method: Name,
        type_arguments: Vec<TypeRefId>,
        arguments: Vec<ExprId>,
    },
    Field {
        receiver: ExprId,
        name: Name,
    },
    Unary {
        op: UnaryOp,
        operand: ExprId,
    },
    Binary {
        op: BinaryOp,
        left: ExprId,
        right: ExprId,
    },
    Assign {
        target: ExprId,
        value: ExprId,
    },
    RecordLiteral {
        ty: Path,
        fields: Vec<RecordFieldValue>,
    },
    Await(ExprId),
    /// Compiler-known timer task constructor: `Task.sleep(milliseconds)`.
    Sleep(Vec<ExprId>),
    /// Compiler-known Task join constructor. Unlike tuple/list literals, this
    /// value carries scheduler mode semantics and must be consumed by await.
    TaskJoin {
        mode: TaskJoinMode,
        arguments: Vec<ExprId>,
    },
    Propagate(ExprId),
    Return(Option<ExprId>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskJoinMode {
    All,
    Settled,
    Any,
    Race,
}

#[derive(Clone, Debug)]
pub enum Statement {
    Let {
        local: LocalId,
        value: ExprId,
    },
    LetTuple {
        locals: Vec<LocalId>,
        value: ExprId,
    },
    Scoped {
        local: LocalId,
        value: ExprId,
    },
    /// Iterates an immutable `Int` binding over `[start, end)`.
    ForRange {
        local: LocalId,
        start: ExprId,
        end: ExprId,
        body: ExprId,
    },
    Defer {
        body: ExprId,
    },
    Discard(ExprId),
    Expr(ExprId),
    Assert(ExprId),
}

#[derive(Clone, Debug)]
pub struct MatchArm {
    pub pattern: PatternId,
    pub value: ExprId,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum Pattern {
    Error,
    Wildcard,
    Binding(LocalId),
    Literal(Literal),
    /// A source name-pattern whose final meaning depends on the visible enum
    /// variants. `binding` is preallocated only for a bare, payload-free name;
    /// semantic resolution uses it when no visible nullary variant wins.
    Name {
        path: Path,
        payload: Vec<PatternId>,
        binding: Option<LocalId>,
    },
    Variant {
        path: Path,
        payload: Vec<PatternId>,
    },
}

#[derive(Clone, Debug)]
pub struct RecordFieldValue {
    pub name: Name,
    pub value: ExprId,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Literal {
    Bool(bool),
    Int(String),
    Float(String),
    Text(String),
    Unit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnaryOp {
    Negate,
    Not,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

/// All lowered HIR for a compilation input.
#[derive(Clone, Debug, Default)]
pub struct Program {
    pub modules: Arena<ModuleId, Module>,
    pub definitions: Arena<DefId, Definition>,
    pub generic_params: Arena<GenericParamId, GenericParam>,
    pub params: Arena<ParamId, Param>,
    pub type_refs: Arena<TypeRefId, TypeRef>,
    pub bodies: Arena<BodyId, Body>,
    pub source_map: ProgramSourceMap,
    module_by_identity: BTreeMap<(PackageId, ModuleName), ModuleId>,
    packages: BTreeMap<PackageId, PackageInfo>,
    root_package: Option<PackageId>,
}

/// Resolved package namespace available during semantic name resolution.
#[derive(Clone, Debug, Default)]
struct PackageInfo {
    dependencies: BTreeMap<Name, PackageId>,
}

/// Result of resolving a source module path from a package-owned module.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModuleResolution {
    Found(ModuleId),
    /// The module exists in the graph but its package is not a direct
    /// dependency of the importing package.
    UndeclaredDependency(PackageId),
    Missing,
}

impl Program {
    #[must_use]
    pub fn module_by_name(&self, name: &ModuleName) -> Option<ModuleId> {
        let mut matches = self
            .module_by_identity
            .iter()
            .filter_map(|((_, candidate), module)| (candidate == name).then_some(*module));
        let first = matches.next()?;
        matches.next().is_none().then_some(first)
    }

    /// Registers the direct dependency aliases visible from one package.
    pub fn register_package(
        &mut self,
        package: PackageId,
        dependencies: impl IntoIterator<Item = (Name, PackageId)>,
        is_root: bool,
    ) {
        self.packages.insert(
            package.clone(),
            PackageInfo {
                dependencies: dependencies.into_iter().collect(),
            },
        );
        if is_root {
            self.root_package = Some(package);
        }
    }

    #[must_use]
    pub fn root_package(&self) -> Option<&PackageId> {
        self.root_package.as_ref()
    }

    #[must_use]
    pub fn is_root_module(&self, module: ModuleId) -> bool {
        self.root_package
            .as_ref()
            .is_none_or(|root| root == &self.modules[module].package)
    }

    /// Resolves a module using only the current package and its declared
    /// direct dependency aliases. Transitive packages are deliberately not in
    /// this lookup scope.
    #[must_use]
    pub fn resolve_module_from(&self, from: ModuleId, requested: &ModuleName) -> ModuleResolution {
        let package = &self.modules[from].package;
        if let Some(module) = self
            .module_by_identity
            .get(&(package.clone(), requested.clone()))
            .copied()
        {
            return ModuleResolution::Found(module);
        }

        let first = requested.as_str().split('.').next().unwrap_or_default();
        if let Some(target_package) = self
            .packages
            .get(package)
            .and_then(|info| info.dependencies.get(&Name::new(first)))
        {
            let target_name = if first == target_package.name() {
                requested.clone()
            } else {
                ModuleName::new(
                    std::iter::once(target_package.name())
                        .chain(requested.as_str().split('.').skip(1))
                        .collect::<Vec<_>>()
                        .join("."),
                )
            };
            return self
                .module_by_identity
                .get(&(target_package.clone(), target_name))
                .copied()
                .map_or(ModuleResolution::Missing, ModuleResolution::Found);
        }

        let transitive =
            self.module_by_identity
                .iter()
                .find_map(|((candidate_package, candidate_name), _)| {
                    (candidate_package != package
                        && (candidate_name == requested
                            || candidate_package.name() == first
                                && candidate_name
                                    .as_str()
                                    .split('.')
                                    .skip(1)
                                    .eq(requested.as_str().split('.').skip(1))))
                    .then_some(candidate_package.clone())
                });
        transitive.map_or(
            ModuleResolution::Missing,
            ModuleResolution::UndeclaredDependency,
        )
    }

    /// Interns a module name, merging files that declare the same module.
    pub fn intern_module(&mut self, name: ModuleName, file: FileId, declaration: Span) -> ModuleId {
        self.intern_package_module(PackageId::legacy(), name, file, declaration)
    }

    /// Interns a package-qualified module, never merging equal source module
    /// names that originate from different resolved packages.
    pub fn intern_package_module(
        &mut self,
        package: PackageId,
        name: ModuleName,
        file: FileId,
        declaration: Span,
    ) -> ModuleId {
        let identity = (package.clone(), name.clone());
        if let Some(module) = self.module_by_identity.get(&identity).copied() {
            if !self.modules[module].files.contains(&file) {
                self.modules[module].files.push(file);
                self.modules[module].files.sort_unstable();
            }
            self.source_map.add_module_declaration(module, declaration);
            return module;
        }

        let module = self.modules.alloc(Module {
            package,
            name,
            files: vec![file],
            imports: Vec::new(),
            items: Vec::new(),
        });
        self.module_by_identity.insert(identity, module);
        self.source_map.add_module_declaration(module, declaration);
        module
    }

    pub fn alloc_definition(&mut self, definition: Definition, span: Span) -> DefId {
        let module = definition.module;
        let id = self.alloc_member_definition(definition, span);
        self.modules[module].items.push(id);
        id
    }

    /// Allocates a top-level definition shell so nested bodies and members can
    /// refer to their owner before the full definition has been lowered.
    pub fn alloc_definition_shell(
        &mut self,
        module: ModuleId,
        name: Option<Name>,
        visibility: Visibility,
        span: Span,
    ) -> DefId {
        self.alloc_definition(
            Definition {
                module,
                name,
                visibility,
                kind: DefinitionKind::Error,
            },
            span,
        )
    }

    pub fn alloc_member_definition_shell(
        &mut self,
        module: ModuleId,
        name: Option<Name>,
        visibility: Visibility,
        span: Span,
    ) -> DefId {
        self.alloc_member_definition(
            Definition {
                module,
                name,
                visibility,
                kind: DefinitionKind::Error,
            },
            span,
        )
    }

    pub fn replace_definition_kind(&mut self, definition: DefId, kind: DefinitionKind) {
        self.definitions[definition].kind = kind;
    }

    /// Allocates a nested definition without exposing it in the module's
    /// top-level namespace. Fields, variants, methods and associated types use
    /// this entry point.
    pub fn alloc_member_definition(&mut self, definition: Definition, span: Span) -> DefId {
        let id = self.definitions.alloc(definition);
        self.source_map.insert_definition(id, span);
        id
    }

    pub fn alloc_type_ref(&mut self, ty: TypeRef, span: Span) -> TypeRefId {
        let id = self.type_refs.alloc(ty);
        self.source_map.insert_type_ref(id, span);
        id
    }

    pub fn alloc_generic_param(&mut self, parameter: GenericParam, span: Span) -> GenericParamId {
        let id = self.generic_params.alloc(parameter);
        self.source_map.insert_generic_param(id, span);
        id
    }

    pub fn alloc_param(&mut self, parameter: Param, span: Span) -> ParamId {
        let id = self.params.alloc(parameter);
        self.source_map.insert_param(id, span);
        id
    }

    pub fn alloc_body(&mut self, body: Body, span: Span) -> BodyId {
        let id = self.bodies.alloc(body);
        self.source_map.insert_body(id, span);
        id
    }
}

#[cfg(test)]
mod tests {
    use loom_core::{FileId, ModuleName, Span};

    use super::Program;

    #[test]
    fn modules_merge_files_without_using_file_order_as_identity() {
        let mut program = Program::default();
        let name = ModuleName::new("shop.order");
        let first = program.intern_module(name.clone(), FileId(7), Span::new(FileId(7), 0, 17));
        let second = program.intern_module(name, FileId(2), Span::new(FileId(2), 0, 17));

        assert_eq!(first, second);
        assert_eq!(program.modules[first].files, vec![FileId(2), FileId(7)]);
        assert_eq!(program.source_map.module_declarations(first).len(), 2);
    }
}
