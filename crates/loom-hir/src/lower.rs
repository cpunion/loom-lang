//! Deterministic lowering from the recovering source AST into source-independent HIR.

use std::collections::{BTreeMap, BTreeSet};

use loom_core::{Diagnostic, FileId, ModuleName, Name, PackageId, Span, TextRange};
use loom_syntax::ast as syntax;

use crate::{
    AssociatedBindingRef, AssociatedTypeDef, BodyBuilder, BodyId, BodyKind, CallableSignature,
    ConceptDef, ConceptRef, ConformanceDef, Contracts, DefId, DefinitionKind, EnumDef, Expr,
    ExprId, FieldDef, FunctionDef, GenericParam, GenericParamId, ImplDef, Import, Literal, Local,
    MatchArm, MethodDef, ModuleId, Param, Path, PathSegment, Pattern, ReceiverKind, RecordDef,
    RecordFieldValue, RefinedTypeDef, Statement, TypeArgumentRef, TypeRef, TypeRefId, UnaryOp,
    VariantDef, Visibility,
};

/// One parsed file supplied to HIR lowering.
#[derive(Clone, Copy)]
pub struct SourceUnit<'a> {
    pub file: FileId,
    pub syntax: &'a syntax::SourceFile,
}

/// A parsed file with a caller-validated resolved package identity.
///
/// This is a trusted compiler-embedding boundary. [`PackageId`] is nominal
/// identity rather than proof of source ownership, so callers accepting
/// filesystem, registry, or artifact inputs must validate that package graph
/// before constructing this unit.
#[derive(Clone)]
pub struct PackageSourceUnit<'a> {
    pub file: FileId,
    pub package: PackageId,
    pub syntax: &'a syntax::SourceFile,
}

/// HIR plus lowering-only diagnostics. Parser diagnostics remain owned by the
/// caller so they are never duplicated or reordered here.
#[derive(Clone, Debug, Default)]
pub struct LoweringResult {
    pub program: crate::Program,
    pub diagnostics: Vec<Diagnostic>,
}

/// Lowers files in stable `FileId` order, independent of discovery order.
#[must_use]
pub fn lower_files<'a>(files: impl IntoIterator<Item = SourceUnit<'a>>) -> LoweringResult {
    lower_package_files(files.into_iter().map(|unit| PackageSourceUnit {
        file: unit.file,
        package: PackageId::legacy(),
        syntax: unit.syntax,
    }))
}

/// Lowers files while preserving package identity in every module.
///
/// This function does not load or authenticate packages and does not enforce
/// reserved-package policy. Callers must supply a previously validated closed
/// package graph; untrusted project inputs should enter through the driver.
#[must_use]
pub fn lower_package_files<'a>(
    files: impl IntoIterator<Item = PackageSourceUnit<'a>>,
) -> LoweringResult {
    let mut files = files.into_iter().collect::<Vec<_>>();
    files.sort_by_key(|unit| unit.file);

    let mut context = LowerContext::default();
    let mut modules = BTreeMap::new();
    let mut seen_files = BTreeSet::new();

    for unit in &files {
        if !seen_files.insert(unit.file) {
            context.diagnostics.push(Diagnostic::error(
                "DuplicateFileInput",
                format!("file id {} was supplied more than once", unit.file.0),
                Span {
                    file: unit.file,
                    range: unit.syntax.range,
                },
            ));
            continue;
        }
        let Some(declaration) = &unit.syntax.module else {
            // The parser already emits ExpectedModule. Keeping the file out of
            // HIR prevents a made-up module identity from leaking downstream.
            continue;
        };
        if declaration.name.segments.is_empty() {
            continue;
        }
        let module = context.program.intern_package_module(
            unit.package.clone(),
            ModuleName::new(declaration.name.as_string()),
            unit.file,
            span(unit.file, declaration.range),
        );
        modules.insert(unit.file, module);
    }

    seen_files.clear();
    for unit in files {
        if !seen_files.insert(unit.file) {
            continue;
        }
        let Some(module) = modules.get(&unit.file).copied() else {
            continue;
        };
        context.lower_file(unit.file, module, unit.syntax);
    }

    LoweringResult {
        program: context.program,
        diagnostics: context.diagnostics,
    }
}

#[derive(Default)]
struct LowerContext {
    program: crate::Program,
    diagnostics: Vec<Diagnostic>,
}

impl LowerContext {
    fn lower_file(&mut self, file: FileId, module: ModuleId, source: &syntax::SourceFile) {
        for import in &source.imports {
            self.program.modules[module].imports.push(Import {
                path: lower_path(file, &import.path),
                span: span(file, import.range),
            });
        }
        for declaration in &source.declarations {
            self.lower_declaration(file, module, declaration);
        }
    }

    fn lower_declaration(&mut self, file: FileId, module: ModuleId, declaration: &syntax::Decl) {
        let visibility = lower_visibility(declaration.visibility);
        let declaration_span = span(file, declaration.range);
        match &declaration.kind {
            syntax::DeclKind::ConstrainedType(source) => {
                let owner = self.program.alloc_definition_shell(
                    module,
                    Some(Name::new(source.name.text.clone())),
                    visibility,
                    declaration_span,
                );
                let base = self.lower_type(file, &source.base);
                let predicate = self.lower_expression_body(
                    file,
                    owner,
                    BodyKind::RefinementPredicate,
                    &source.predicate,
                );
                self.program.replace_definition_kind(
                    owner,
                    DefinitionKind::RefinedType(RefinedTypeDef { base, predicate }),
                );
            }
            syntax::DeclKind::Record(source) => {
                self.lower_record(file, module, visibility, declaration_span, source);
            }
            syntax::DeclKind::Enum(source) => {
                self.lower_enum(file, module, visibility, declaration_span, source);
            }
            syntax::DeclKind::Function(source) => {
                self.lower_function(file, module, visibility, declaration_span, source);
            }
            syntax::DeclKind::Impl(source) => {
                self.lower_impl(file, module, declaration_span, source);
            }
            syntax::DeclKind::Concept(source) => {
                self.lower_concept(file, module, visibility, declaration_span, source);
            }
            syntax::DeclKind::Error(_) => {
                self.program
                    .alloc_definition_shell(module, None, visibility, declaration_span);
            }
        }
    }

    fn lower_record(
        &mut self,
        file: FileId,
        module: ModuleId,
        visibility: Visibility,
        declaration_span: Span,
        source: &syntax::RecordDecl,
    ) {
        let owner = self.program.alloc_definition_shell(
            module,
            Some(Name::new(source.name.text.clone())),
            visibility,
            declaration_span,
        );
        let generic_params = self.lower_generic_params(file, owner, &source.generics);
        let fields = source
            .fields
            .iter()
            .map(|field| {
                let ty = self.lower_type(file, &field.ty);
                self.program.alloc_member_definition(
                    crate::Definition {
                        module,
                        name: Some(Name::new(field.name.text.clone())),
                        visibility: Visibility::Public,
                        kind: DefinitionKind::Field(FieldDef { owner, ty }),
                    },
                    span(file, field.range),
                )
            })
            .collect();
        let invariant = source.invariant.as_ref().map(|predicate| {
            self.lower_expression_body(file, owner, BodyKind::RecordInvariant, predicate)
        });
        self.program.replace_definition_kind(
            owner,
            DefinitionKind::Record(RecordDef {
                generic_params,
                fields,
                invariant,
            }),
        );
    }

    fn lower_enum(
        &mut self,
        file: FileId,
        module: ModuleId,
        visibility: Visibility,
        declaration_span: Span,
        source: &syntax::EnumDecl,
    ) {
        let owner = self.program.alloc_definition_shell(
            module,
            Some(Name::new(source.name.text.clone())),
            visibility,
            declaration_span,
        );
        let generic_params = self.lower_generic_params(file, owner, &source.generics);
        let variants = source
            .variants
            .iter()
            .map(|variant| {
                let payload = variant
                    .payload
                    .iter()
                    .map(|ty| self.lower_type(file, ty))
                    .collect();
                self.program.alloc_member_definition(
                    crate::Definition {
                        module,
                        name: Some(Name::new(variant.name.text.clone())),
                        visibility: Visibility::Public,
                        kind: DefinitionKind::Variant(VariantDef { owner, payload }),
                    },
                    span(file, variant.range),
                )
            })
            .collect();
        self.program.replace_definition_kind(
            owner,
            DefinitionKind::Enum(EnumDef {
                generic_params,
                variants,
            }),
        );
    }

    fn lower_function(
        &mut self,
        file: FileId,
        module: ModuleId,
        visibility: Visibility,
        declaration_span: Span,
        source: &syntax::FunctionDecl,
    ) {
        let owner = self.program.alloc_definition_shell(
            module,
            Some(Name::new(source.signature.name.text.clone())),
            visibility,
            declaration_span,
        );
        let signature = self.lower_signature(file, owner, &source.signature, false);
        let body = self.lower_block_body(file, owner, BodyKind::Function, &source.body);
        let function = FunctionDef {
            signature,
            body,
            is_async: source.is_async,
        };
        self.program.replace_definition_kind(
            owner,
            if source.is_test {
                DefinitionKind::Test(function)
            } else {
                DefinitionKind::Function(function)
            },
        );
    }

    fn lower_impl(
        &mut self,
        file: FileId,
        module: ModuleId,
        declaration_span: Span,
        source: &syntax::ImplDecl,
    ) {
        let owner = self.program.alloc_definition_shell(
            module,
            None,
            Visibility::Private,
            declaration_span,
        );
        let generic_params = self.lower_generic_params(file, owner, &source.generics);
        match &source.kind {
            syntax::ImplKind::Inherent { target, methods } => {
                let target = self.lower_type(file, target);
                let methods = methods
                    .iter()
                    .map(|method| self.lower_method(file, module, owner, method, true))
                    .collect();
                self.program.replace_definition_kind(
                    owner,
                    DefinitionKind::InherentImpl(ImplDef {
                        generic_params,
                        target,
                        methods,
                    }),
                );
            }
            syntax::ImplKind::Conformance {
                concept,
                target,
                members,
            } => {
                let concept = self.lower_concept_ref(file, concept);
                let target = self.lower_type(file, target);
                let mut associated_types = Vec::new();
                let mut methods = Vec::new();
                for member in members {
                    match member {
                        syntax::ConformanceMember::AssociatedType(binding) => {
                            let value = self.lower_type(file, &binding.value);
                            associated_types.push(self.program.alloc_member_definition(
                                crate::Definition {
                                    module,
                                    name: Some(Name::new(binding.name.text.clone())),
                                    visibility: Visibility::Private,
                                    kind: DefinitionKind::AssociatedType(AssociatedTypeDef {
                                        owner,
                                        bounds: Vec::new(),
                                        binding: Some(value),
                                    }),
                                },
                                span(file, binding.range),
                            ));
                        }
                        syntax::ConformanceMember::Method(method) => {
                            methods.push(self.lower_method(file, module, owner, method, true));
                        }
                        syntax::ConformanceMember::Error(error) => {
                            self.program.alloc_member_definition_shell(
                                module,
                                None,
                                Visibility::Private,
                                span(file, error.range),
                            );
                        }
                    }
                }
                self.program.replace_definition_kind(
                    owner,
                    DefinitionKind::Conformance(ConformanceDef {
                        generic_params,
                        concept,
                        target,
                        associated_types,
                        methods,
                    }),
                );
            }
        }
    }

    fn lower_concept(
        &mut self,
        file: FileId,
        module: ModuleId,
        visibility: Visibility,
        declaration_span: Span,
        source: &syntax::ConceptDecl,
    ) {
        let owner = self.program.alloc_definition_shell(
            module,
            Some(Name::new(source.name.text.clone())),
            visibility,
            declaration_span,
        );
        let mut associated_types = Vec::new();
        let mut requirements = Vec::new();
        for member in &source.members {
            match member {
                syntax::ConceptMember::AssociatedType(associated) => {
                    let bounds = associated
                        .bounds
                        .iter()
                        .map(|bound| self.lower_concept_ref(file, bound))
                        .collect();
                    associated_types.push(self.program.alloc_member_definition(
                        crate::Definition {
                            module,
                            name: Some(Name::new(associated.name.text.clone())),
                            visibility: Visibility::Public,
                            kind: DefinitionKind::AssociatedType(AssociatedTypeDef {
                                owner,
                                bounds,
                                binding: None,
                            }),
                        },
                        span(file, associated.range),
                    ));
                }
                syntax::ConceptMember::Method(requirement) => {
                    let method = self.program.alloc_member_definition_shell(
                        module,
                        Some(Name::new(requirement.signature.name.text.clone())),
                        Visibility::Public,
                        span(file, requirement.range),
                    );
                    let signature = self.lower_signature(
                        file,
                        method,
                        &requirement.signature,
                        requirement.is_static,
                    );
                    self.program.replace_definition_kind(
                        method,
                        DefinitionKind::Method(MethodDef {
                            owner,
                            signature,
                            body: None,
                        }),
                    );
                    requirements.push(method);
                }
                syntax::ConceptMember::Error(error) => {
                    self.program.alloc_member_definition_shell(
                        module,
                        None,
                        Visibility::Private,
                        span(file, error.range),
                    );
                }
            }
        }
        self.program.replace_definition_kind(
            owner,
            DefinitionKind::Concept(ConceptDef {
                dyn_capable: source.dynamic,
                associated_types,
                requirements,
            }),
        );
    }

    fn lower_method(
        &mut self,
        file: FileId,
        module: ModuleId,
        owner: DefId,
        source: &syntax::MethodDecl,
        allow_static: bool,
    ) -> DefId {
        let method = self.program.alloc_member_definition_shell(
            module,
            Some(Name::new(source.signature.name.text.clone())),
            lower_visibility(source.visibility),
            span(file, source.range),
        );
        let signature = self.lower_signature(
            file,
            method,
            &source.signature,
            allow_static && source.is_static,
        );
        let body = self.lower_block_body(file, method, BodyKind::Method, &source.body);
        self.program.replace_definition_kind(
            method,
            DefinitionKind::Method(MethodDef {
                owner,
                signature,
                body: Some(body),
            }),
        );
        method
    }

    fn lower_signature(
        &mut self,
        file: FileId,
        owner: DefId,
        source: &syntax::CallableSignature,
        force_static: bool,
    ) -> CallableSignature {
        let generic_params = self.lower_generic_params(file, owner, &source.generics);
        let receiver = if force_static {
            Some(ReceiverKind::Static)
        } else {
            source.receiver.map(|receiver| match receiver {
                syntax::Receiver::ReadOnly(_) => ReceiverKind::ReadOnly,
                syntax::Receiver::Mutable(_) => ReceiverKind::Mutable,
            })
        };
        let params = source
            .parameters
            .iter()
            .map(|parameter| {
                let ty = self.lower_type(file, &parameter.ty);
                self.program.alloc_param(
                    Param {
                        owner,
                        name: Name::new(parameter.name.text.clone()),
                        ty,
                    },
                    span(file, parameter.range),
                )
            })
            .collect();
        let return_ty = source
            .return_type
            .as_ref()
            .map(|ty| self.lower_type(file, ty));
        let mut contracts = Contracts::default();
        for contract in &source.contracts {
            let (kind, destination) = match contract.kind {
                syntax::ContractKind::Requires => (BodyKind::Requires, &mut contracts.requires),
                syntax::ContractKind::Ensures => (BodyKind::Ensures, &mut contracts.ensures),
            };
            destination.push(self.lower_expression_body(file, owner, kind, &contract.predicate));
        }
        CallableSignature {
            generic_params,
            receiver,
            params,
            return_ty,
            contracts,
        }
    }

    fn lower_generic_params(
        &mut self,
        file: FileId,
        owner: DefId,
        source: &[syntax::GenericParam],
    ) -> Vec<GenericParamId> {
        source
            .iter()
            .map(|parameter| {
                let bounds = parameter
                    .bounds
                    .iter()
                    .map(|bound| self.lower_concept_ref(file, bound))
                    .collect();
                self.program.alloc_generic_param(
                    GenericParam {
                        owner,
                        name: Name::new(parameter.name.text.clone()),
                        bounds,
                    },
                    span(file, parameter.range),
                )
            })
            .collect()
    }

    fn lower_concept_ref(&mut self, file: FileId, source: &syntax::ConceptRef) -> ConceptRef {
        lower_concept_ref_into(&mut self.program, file, source)
    }

    fn lower_type(&mut self, file: FileId, source: &syntax::TypeExpr) -> TypeRefId {
        lower_type_into(&mut self.program, file, source)
    }

    fn lower_expression_body(
        &mut self,
        file: FileId,
        owner: DefId,
        kind: BodyKind,
        source: &syntax::Expr,
    ) -> BodyId {
        let mut body = BodyLower::new(file, owner, kind, &mut self.program, &mut self.diagnostics);
        let root = body.lower_expr(source);
        let lowered = body.builder.finish(root);
        self.program.alloc_body(lowered, span(file, source.range))
    }

    fn lower_block_body(
        &mut self,
        file: FileId,
        owner: DefId,
        kind: BodyKind,
        source: &syntax::Block,
    ) -> BodyId {
        let mut body = BodyLower::new(file, owner, kind, &mut self.program, &mut self.diagnostics);
        let root = body.lower_block(source);
        let lowered = body.builder.finish(root);
        self.program.alloc_body(lowered, span(file, source.range))
    }
}

struct BodyLower<'a> {
    file: FileId,
    builder: BodyBuilder,
    program: &'a mut crate::Program,
    diagnostics: &'a mut Vec<Diagnostic>,
}

impl<'a> BodyLower<'a> {
    fn new(
        file: FileId,
        owner: DefId,
        kind: BodyKind,
        program: &'a mut crate::Program,
        diagnostics: &'a mut Vec<Diagnostic>,
    ) -> Self {
        Self {
            file,
            builder: BodyBuilder::new(owner, kind),
            program,
            diagnostics,
        }
    }

    fn lower_block(&mut self, source: &syntax::Block) -> ExprId {
        let mut statements = Vec::new();
        let mut tail = None;
        let last = source.items.len().saturating_sub(1);
        for (index, item) in source.items.iter().enumerate() {
            match item {
                syntax::BlockItem::Local(binding) => {
                    let value = self.lower_expr(&binding.value);
                    let annotation = binding
                        .annotation
                        .as_ref()
                        .map(|annotation| self.lower_type(annotation));
                    let locals = binding
                        .names
                        .iter()
                        .map(|name| {
                            self.builder.alloc_local(
                                Local {
                                    name: Name::new(name.text.clone()),
                                    mutable: binding.mutable,
                                    annotation,
                                },
                                self.span(name.range),
                            )
                        })
                        .collect::<Vec<_>>();
                    statements.push(if locals.len() > 1 {
                        Statement::LetTuple { locals, value }
                    } else if binding.scoped {
                        Statement::Scoped {
                            local: locals[0],
                            value,
                        }
                    } else {
                        Statement::Let {
                            local: locals[0],
                            value,
                        }
                    });
                }
                syntax::BlockItem::ForRange(loop_) => {
                    let local = self.builder.alloc_local(
                        Local {
                            name: Name::new(loop_.binding.text.clone()),
                            mutable: false,
                            annotation: None,
                        },
                        self.span(loop_.binding.range),
                    );
                    let start = self.lower_expr(&loop_.start);
                    let end = self.lower_expr(&loop_.end);
                    let body = self.lower_block(&loop_.body);
                    statements.push(Statement::ForRange {
                        local,
                        start,
                        end,
                        body,
                    });
                }
                syntax::BlockItem::Defer(block) => {
                    statements.push(Statement::Defer {
                        body: self.lower_block(block),
                    });
                }
                syntax::BlockItem::Discard(expression) => {
                    statements.push(Statement::Discard(self.lower_expr(expression)));
                }
                syntax::BlockItem::Return(returned) => {
                    let value = returned.value.as_ref().map(|value| self.lower_expr(value));
                    let expression = self
                        .builder
                        .alloc_expr(Expr::Return(value), self.span(returned.range));
                    statements.push(Statement::Expr(expression));
                }
                syntax::BlockItem::Assert(predicate) => {
                    statements.push(Statement::Assert(self.lower_expr(predicate)));
                }
                syntax::BlockItem::Assignment(assignment) => {
                    let target = self.lower_expr(&assignment.target);
                    let value = self.lower_expr(&assignment.value);
                    let expression = self
                        .builder
                        .alloc_expr(Expr::Assign { target, value }, self.span(assignment.range));
                    statements.push(Statement::Expr(expression));
                }
                syntax::BlockItem::Expr(expression) if index == last => {
                    tail = Some(self.lower_expr(expression));
                }
                syntax::BlockItem::Expr(expression) => {
                    statements.push(Statement::Expr(self.lower_expr(expression)));
                }
                syntax::BlockItem::Error(error) => {
                    let expression = self.builder.alloc_expr(Expr::Error, self.span(error.range));
                    statements.push(Statement::Expr(expression));
                }
            }
        }
        self.builder
            .alloc_expr(Expr::Block { statements, tail }, self.span(source.range))
    }

    #[allow(clippy::too_many_lines)]
    fn lower_expr(&mut self, source: &syntax::Expr) -> ExprId {
        let expression = match &source.kind {
            syntax::ExprKind::Literal(literal) => Expr::Literal(lower_literal(literal)),
            syntax::ExprKind::Tuple(elements) => Expr::Tuple(
                elements
                    .iter()
                    .map(|element| self.lower_expr(element))
                    .collect(),
            ),
            syntax::ExprKind::List(elements) => Expr::List(
                elements
                    .iter()
                    .map(|element| self.lower_expr(element))
                    .collect(),
            ),
            syntax::ExprKind::Name(path) => Expr::Path(lower_path(self.file, path)),
            syntax::ExprKind::SelfValue => Expr::SelfValue,
            syntax::ExprKind::ContractResult => Expr::ResultValue,
            syntax::ExprKind::Old(value) => Expr::Old(self.lower_expr(value)),
            syntax::ExprKind::Unary { op, operand } => Expr::Unary {
                op: lower_unary(*op),
                operand: self.lower_expr(operand),
            },
            syntax::ExprKind::Binary { op, left, right } => Expr::Binary {
                op: lower_binary(*op),
                left: self.lower_expr(left),
                right: self.lower_expr(right),
            },
            syntax::ExprKind::Member { receiver, name } => Expr::Field {
                receiver: self.lower_expr(receiver),
                name: Name::new(name.text.clone()),
            },
            syntax::ExprKind::QualifiedMember { .. } => {
                self.diagnostics.push(Diagnostic::error(
                    "QualifiedMethodWithoutCall",
                    "a qualified concept method selection must be called",
                    self.span(source.range),
                ));
                Expr::Error
            }
            syntax::ExprKind::Call {
                callee,
                type_arguments,
                arguments,
            } => self.lower_call(callee, type_arguments, arguments),
            syntax::ExprKind::Await(value) => Expr::Await(self.lower_expr(value)),
            syntax::ExprKind::Propagate(value) => Expr::Propagate(self.lower_expr(value)),
            syntax::ExprKind::RecordLiteral {
                constructor,
                fields,
            } => Expr::RecordLiteral {
                ty: lower_path(self.file, constructor),
                fields: fields
                    .iter()
                    .map(|field| RecordFieldValue {
                        name: Name::new(field.name.text.clone()),
                        value: self.lower_expr(&field.value),
                        span: self.span(field.range),
                    })
                    .collect(),
            },
            syntax::ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let condition = self.lower_expr(condition);
                let then_branch = self.lower_block(then_branch);
                let else_branch = else_branch.as_ref().map(|branch| match branch {
                    syntax::ElseBranch::Block(block) => self.lower_block(block),
                    syntax::ElseBranch::If(expression) => self.lower_expr(expression),
                });
                Expr::If {
                    condition,
                    then_branch,
                    else_branch,
                }
            }
            syntax::ExprKind::Match { scrutinee, arms } => Expr::Match {
                scrutinee: self.lower_expr(scrutinee),
                arms: arms
                    .iter()
                    .map(|arm| MatchArm {
                        pattern: self.lower_pattern(&arm.pattern),
                        value: self.lower_expr(&arm.value),
                        span: self.span(arm.range),
                    })
                    .collect(),
            },
            syntax::ExprKind::Block(block) => return self.lower_block(block),
            syntax::ExprKind::Error => Expr::Error,
        };
        self.builder.alloc_expr(expression, self.span(source.range))
    }

    fn lower_call(
        &mut self,
        callee: &syntax::Expr,
        type_arguments: &[syntax::TypeExpr],
        arguments: &[syntax::Expr],
    ) -> Expr {
        let type_arguments: Vec<_> = type_arguments
            .iter()
            .map(|argument| self.lower_type(argument))
            .collect();
        let arguments = arguments
            .iter()
            .map(|argument| self.lower_expr(argument))
            .collect::<Vec<_>>();
        match &callee.kind {
            syntax::ExprKind::Member { receiver, name } => Expr::MethodCall {
                receiver: self.lower_expr(receiver),
                method: Name::new(name.text.clone()),
                type_arguments,
                arguments,
            },
            syntax::ExprKind::QualifiedMember {
                base,
                concept,
                name,
            } => Expr::QualifiedMethodCall {
                self_ty: self.lower_type(base),
                concept: self.lower_concept_ref(concept),
                method: Name::new(name.text.clone()),
                type_arguments,
                arguments,
            },
            _ => Expr::Call {
                callee: self.lower_expr(callee),
                type_arguments,
                arguments,
            },
        }
    }

    fn lower_pattern(&mut self, source: &syntax::Pattern) -> crate::PatternId {
        let pattern = match &source.kind {
            syntax::PatternKind::Wildcard => Pattern::Wildcard,
            syntax::PatternKind::Literal(literal) => Pattern::Literal(lower_literal(literal)),
            syntax::PatternKind::Name { path, payload } => {
                let payload = payload
                    .iter()
                    .map(|pattern| self.lower_pattern(pattern))
                    .collect::<Vec<_>>();
                let binding = if path.segments.len() == 1 && payload.is_empty() {
                    Some(self.builder.alloc_local(
                        Local {
                            name: Name::new(path.segments[0].text.clone()),
                            mutable: false,
                            annotation: None,
                        },
                        self.span(source.range),
                    ))
                } else {
                    None
                };
                Pattern::Name {
                    path: lower_path(self.file, path),
                    payload,
                    binding,
                }
            }
            syntax::PatternKind::Error => Pattern::Error,
        };
        self.builder.alloc_pattern(pattern, self.span(source.range))
    }

    fn lower_type(&mut self, source: &syntax::TypeExpr) -> TypeRefId {
        lower_type_into(self.program, self.file, source)
    }

    fn lower_concept_ref(&mut self, source: &syntax::ConceptRef) -> ConceptRef {
        lower_concept_ref_into(self.program, self.file, source)
    }

    const fn span(&self, range: TextRange) -> Span {
        span(self.file, range)
    }
}

const fn span(file: FileId, range: TextRange) -> Span {
    Span { file, range }
}

fn lower_concept_ref_into(
    program: &mut crate::Program,
    file: FileId,
    source: &syntax::ConceptRef,
) -> ConceptRef {
    ConceptRef {
        path: lower_path(file, &source.path),
        bindings: source
            .bindings
            .iter()
            .map(|binding| AssociatedBindingRef {
                name: Name::new(binding.name.text.clone()),
                ty: lower_type_into(program, file, &binding.ty),
            })
            .collect(),
    }
}

fn lower_type_into(
    program: &mut crate::Program,
    file: FileId,
    source: &syntax::TypeExpr,
) -> TypeRefId {
    let lowered = match &source.kind {
        syntax::TypeExprKind::Tuple(elements) => TypeRef::Tuple(
            elements
                .iter()
                .map(|element| lower_type_into(program, file, element))
                .collect(),
        ),
        syntax::TypeExprKind::Named { path, arguments }
            if arguments.is_empty()
                && path.segments.len() == 1
                && path.segments[0].text == "Self" =>
        {
            TypeRef::SelfType
        }
        syntax::TypeExprKind::Named { path, arguments }
            if arguments.is_empty()
                && path.segments.len() == 2
                && path.segments[0].text == "Self" =>
        {
            let self_ty =
                program.alloc_type_ref(TypeRef::SelfType, span(file, path.segments[0].range));
            TypeRef::Projection {
                self_ty,
                concept: None,
                associated: Name::new(path.segments[1].text.clone()),
            }
        }
        syntax::TypeExprKind::Named { path, arguments } if arguments.is_empty() => {
            TypeRef::Path(lower_path(file, path))
        }
        syntax::TypeExprKind::Named { path, arguments } => TypeRef::Apply {
            constructor: lower_path(file, path),
            arguments: arguments
                .iter()
                .map(|argument| match argument {
                    syntax::TypeArgument::Type(ty) => {
                        TypeArgumentRef::Type(lower_type_into(program, file, ty))
                    }
                    syntax::TypeArgument::Binding(binding) => {
                        TypeArgumentRef::Binding(AssociatedBindingRef {
                            name: Name::new(binding.name.text.clone()),
                            ty: lower_type_into(program, file, &binding.ty),
                        })
                    }
                })
                .collect(),
        },
        syntax::TypeExprKind::QualifiedProjection {
            base,
            concept,
            associated,
        } => TypeRef::Projection {
            self_ty: lower_type_into(program, file, base),
            concept: Some(lower_path(file, &concept.path)),
            associated: Name::new(associated.text.clone()),
        },
        syntax::TypeExprKind::BareDyn(target) => {
            TypeRef::Dyn(lower_concept_ref_into(program, file, target))
        }
        syntax::TypeExprKind::Error => TypeRef::Error,
    };
    program.alloc_type_ref(lowered, span(file, source.range))
}

fn lower_visibility(visibility: syntax::Visibility) -> Visibility {
    match visibility {
        syntax::Visibility::Private => Visibility::Private,
        syntax::Visibility::Public => Visibility::Public,
    }
}

fn lower_path(file: FileId, path: &syntax::Path) -> Path {
    Path {
        segments: path
            .segments
            .iter()
            .map(|segment| PathSegment {
                name: Name::new(segment.text.clone()),
                span: span(file, segment.range),
            })
            .collect(),
    }
}

fn lower_literal(literal: &syntax::Literal) -> Literal {
    match literal {
        syntax::Literal::Int(value) => Literal::Int(value.clone()),
        syntax::Literal::Float(value) => Literal::Float(value.clone()),
        syntax::Literal::Text(value) => Literal::Text(value.clone()),
        syntax::Literal::Bool(value) => Literal::Bool(*value),
    }
}

fn lower_unary(operator: syntax::UnaryOp) -> UnaryOp {
    match operator {
        syntax::UnaryOp::Negate => UnaryOp::Negate,
        syntax::UnaryOp::Not => UnaryOp::Not,
    }
}

fn lower_binary(operator: syntax::BinaryOp) -> crate::BinaryOp {
    match operator {
        syntax::BinaryOp::Multiply => crate::BinaryOp::Multiply,
        syntax::BinaryOp::Divide => crate::BinaryOp::Divide,
        syntax::BinaryOp::Add => crate::BinaryOp::Add,
        syntax::BinaryOp::Subtract => crate::BinaryOp::Subtract,
        syntax::BinaryOp::Less => crate::BinaryOp::Less,
        syntax::BinaryOp::LessEqual => crate::BinaryOp::LessEqual,
        syntax::BinaryOp::Greater => crate::BinaryOp::Greater,
        syntax::BinaryOp::GreaterEqual => crate::BinaryOp::GreaterEqual,
        syntax::BinaryOp::Equal => crate::BinaryOp::Equal,
        syntax::BinaryOp::NotEqual => crate::BinaryOp::NotEqual,
        syntax::BinaryOp::And => crate::BinaryOp::And,
        syntax::BinaryOp::Or => crate::BinaryOp::Or,
    }
}

#[cfg(test)]
mod tests {
    use loom_core::FileId;
    use loom_syntax::parse_with_file;

    use super::{SourceUnit, lower_files};
    use crate::{DefinitionKind, Expr, Pattern, Statement};

    #[test]
    fn lowering_is_deterministic_and_keeps_pattern_name_unresolved() {
        let first = parse_with_file(
            FileId(4),
            "module sample\n\nenum Maybe[T] {\n    None\n    Some(T)\n}\n",
        );
        let second = parse_with_file(
            FileId(2),
            "module sample\n\nfn unwrap[T](value Maybe[T], fallback T) T {\n    match value {\n        Some(item) => item\n        None => fallback\n    }\n}\n",
        );
        assert!(first.diagnostics().is_empty(), "{:?}", first.diagnostics());
        assert!(
            second.diagnostics().is_empty(),
            "{:?}",
            second.diagnostics()
        );

        let lowered = lower_files([
            SourceUnit {
                file: FileId(4),
                syntax: first.ast(),
            },
            SourceUnit {
                file: FileId(2),
                syntax: second.ast(),
            },
        ]);
        let module = lowered.program.modules.iter().next().unwrap().1;
        assert_eq!(module.files, vec![FileId(2), FileId(4)]);
        let function = module
            .items
            .iter()
            .find_map(
                |definition| match &lowered.program.definitions[*definition].kind {
                    DefinitionKind::Function(function) => Some(function),
                    _ => None,
                },
            )
            .unwrap();
        let body = &lowered.program.bodies[function.body];
        assert!(
            body.expressions
                .values()
                .any(|expression| { matches!(expression, Expr::Match { .. }) })
        );
        assert!(body.patterns.values().any(|pattern| {
            matches!(
                pattern,
                Pattern::Name {
                    binding: Some(_),
                    ..
                }
            )
        }));
    }

    #[test]
    fn omitted_returns_remain_fixed_implicit_unit_markers() {
        let parsed = parse_with_file(
            FileId(0),
            r"module returns

record R {}

pub fn omitted() { return }
async fn omittedAsync() {}
test fn omittedTest() { return }
fn anotherOmitted() {}

impl R {
    pub method inherent(self) {}
}

concept C {
    method required(self)
}

impl C for R {
    method required(self) { return }
}
",
        );
        assert!(
            parsed.diagnostics().is_empty(),
            "{:?}",
            parsed.diagnostics()
        );

        let lowered = lower_files([SourceUnit {
            file: FileId(0),
            syntax: parsed.ast(),
        }]);
        assert!(lowered.diagnostics.is_empty(), "{:?}", lowered.diagnostics);

        let mut omitted_signatures = 0;
        for (_, definition) in lowered.program.definitions.iter() {
            let signature = match &definition.kind {
                DefinitionKind::Function(function) | DefinitionKind::Test(function) => {
                    Some(&function.signature)
                }
                DefinitionKind::Method(method) => Some(&method.signature),
                _ => None,
            };
            let Some(signature) = signature else {
                continue;
            };
            assert!(signature.return_ty.is_none());
            omitted_signatures += 1;
        }
        assert_eq!(omitted_signatures, 7);
        assert!(lowered.program.bodies.iter().any(|(_, body)| {
            body.expressions
                .values()
                .any(|expression| matches!(expression, Expr::Return(None)))
        }));
    }

    #[test]
    fn discard_lowers_to_a_distinct_statement() {
        let parsed = parse_with_file(
            FileId(0),
            "module discards\nfn value() Int { 1 }\nfn run() { discard value() }\n",
        );
        assert!(
            parsed.diagnostics().is_empty(),
            "{:?}",
            parsed.diagnostics()
        );
        let lowered = lower_files([SourceUnit {
            file: FileId(0),
            syntax: parsed.ast(),
        }]);
        assert!(lowered.diagnostics.is_empty(), "{:?}", lowered.diagnostics);

        let function = lowered
            .program
            .definitions
            .iter()
            .find_map(|(_, definition)| match &definition.kind {
                DefinitionKind::Function(function)
                    if definition
                        .name
                        .as_ref()
                        .is_some_and(|name| name.as_str() == "run") =>
                {
                    Some(function)
                }
                _ => None,
            })
            .expect("run function");
        let body = &lowered.program.bodies[function.body];
        let Expr::Block { statements, tail } = &body.expressions[body.root] else {
            panic!("expected block root");
        };
        assert!(tail.is_none());
        assert!(matches!(statements.as_slice(), [Statement::Discard(_)]));
    }

    #[test]
    fn task_library_calls_remain_ordinary_method_calls_in_hir() {
        let parsed = parse_with_file(
            FileId(0),
            r"module task_calls

fn calls(task Task[Int]) {
    discard Task.sleep(1)
    discard Task.all(task)
    discard Task.settled(task)
    discard Task.any(task)
    discard Task.race(task)
}
",
        );
        assert!(
            parsed.diagnostics().is_empty(),
            "{:?}",
            parsed.diagnostics()
        );
        let lowered = lower_files([SourceUnit {
            file: FileId(0),
            syntax: parsed.ast(),
        }]);
        assert!(lowered.diagnostics.is_empty(), "{:?}", lowered.diagnostics);

        let function = lowered
            .program
            .definitions
            .iter()
            .find_map(|(_, definition)| match &definition.kind {
                DefinitionKind::Function(function) => Some(function),
                _ => None,
            })
            .expect("calls function");
        let body = &lowered.program.bodies[function.body];
        let mut methods = body
            .expressions
            .values()
            .filter_map(|expression| match expression {
                Expr::MethodCall { method, .. } => Some(method.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        methods.sort_unstable();
        assert_eq!(methods, ["all", "any", "race", "settled", "sleep"]);
    }
}
