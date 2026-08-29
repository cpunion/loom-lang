//! Error-recovering recursive-descent parser.

use loom_core::{Diagnostic, FileId, Span, TextRange};
use serde::{Deserialize, Serialize};

use crate::ast::{
    Assignment, AssociatedBinding, AssociatedTypeBinding, AssociatedTypeRequirement, BinaryOp,
    Block, BlockItem, CallableSignature, ConceptDecl, ConceptMember, ConceptRef, ConformanceMember,
    ConstrainedTypeDecl, Contract, ContractKind, Decl, DeclKind, ElseBranch, EnumDecl, EnumVariant,
    ErrorNode, Expr, ExprKind, ForRange, FunctionDecl, GenericParam, Ident, ImplDecl, ImplKind,
    ImportDecl, Literal, LocalBinding, MatchArm, MethodDecl, MethodRequirement, Parameter, Path,
    Pattern, PatternKind, Receiver, RecordDecl, RecordField, RecordLiteralField, ReturnExpr,
    SourceFile, TypeArgument, TypeExpr, TypeExprKind, UnaryOp, Visibility,
};
use crate::lexer::{Lexed, Token, TokenKind, lex};

/// Version of the public parser nesting-limit contract.
///
/// A change to either the counting rules or [`MAX_SYNTAX_NESTING`] must bump
/// this value so compiler and tooling artifacts can advertise the same limit.
pub const SYNTAX_NESTING_LIMIT_VERSION: u32 = 2;

/// Maximum number of recursive syntactic wrappers accepted in one construct.
///
/// Atomic expressions, types, and patterns do not consume this budget. Unary
/// operators, delimited recursion, type/pattern payloads, and iterative AST
/// wrappers such as calls and member access do. Inputs beyond the limit remain
/// lossless and produce `SyntaxNestingLimit`.
// Keep the recursive-descent stack below Rust's ordinary 2 MiB worker-thread
// stack even for the largest parser frames. Iterative postfix/binary chains
// still share the same budget, so hostile inputs fail before AST teardown can
// recurse deeply enough to abort the process.
pub const MAX_SYNTAX_NESTING: usize = 128;

/// A complete parse: source-shaped AST, lossless tokens, and all diagnostics.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Parse {
    source: String,
    tokens: Vec<Token>,
    ast: SourceFile,
    diagnostics: Vec<Diagnostic>,
}

impl Parse {
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    #[must_use]
    pub fn tokens(&self) -> &[Token] {
        &self.tokens
    }

    #[must_use]
    pub const fn ast(&self) -> &SourceFile {
        &self.ast
    }

    #[must_use]
    pub fn into_ast(self) -> SourceFile {
        self.ast
    }

    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    #[must_use]
    pub fn has_errors(&self) -> bool {
        !self.diagnostics.is_empty()
    }

    /// Reconstructs the input from the token stream. This is useful as a hard
    /// assertion that recovery never discards source text.
    #[must_use]
    pub fn reconstructed(&self) -> String {
        self.tokens
            .iter()
            .map(|token| token.text.as_str())
            .collect()
    }

    /// Rebinds cached diagnostics to the current stable driver file identity.
    pub fn rebind_file(&mut self, file: FileId) {
        for diagnostic in &mut self.diagnostics {
            diagnostic.primary.file = file;
            for label in &mut diagnostic.labels {
                label.span.file = file;
            }
        }
    }

    /// Validates the lossless cache boundary against the exact source bytes.
    #[must_use]
    pub fn is_valid_for_source(&self, source: &str) -> bool {
        if self.source != source || self.reconstructed() != source {
            return false;
        }
        let mut cursor = 0_u32;
        for token in &self.tokens {
            if token.range.start != cursor || token.range.end < token.range.start {
                return false;
            }
            let Ok(start) = usize::try_from(token.range.start) else {
                return false;
            };
            let Ok(end) = usize::try_from(token.range.end) else {
                return false;
            };
            if source.get(start..end) != Some(token.text.as_str()) {
                return false;
            }
            cursor = token.range.end;
        }
        usize::try_from(cursor).ok() == Some(source.len())
            && self.diagnostics.iter().all(|diagnostic| {
                diagnostic.primary.range.end <= cursor
                    && diagnostic
                        .labels
                        .iter()
                        .all(|label| label.span.range.end <= cursor)
            })
    }
}

/// Parses source using `FileId(0)` for diagnostics.
#[must_use]
pub fn parse(source: &str) -> Parse {
    parse_with_file(FileId(0), source)
}

/// Parses source and attaches `file` to every lexical and syntactic diagnostic.
#[must_use]
pub fn parse_with_file(file: FileId, source: &str) -> Parse {
    let Lexed { tokens, errors } = lex(source);
    let mut diagnostics: Vec<_> = errors
        .into_iter()
        .map(|error| {
            Diagnostic::error(
                error.code,
                error.message,
                Span {
                    file,
                    range: error.range,
                },
            )
        })
        .collect();
    let mut parser = Parser::new(file, &tokens);
    let ast = parser.parse_file(source.len());
    diagnostics.extend(parser.diagnostics);
    Parse {
        source: source.to_owned(),
        tokens,
        ast,
        diagnostics,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CallableContext {
    PrivateFunction,
    PublicFunction,
    Test,
    PrivateMethod,
    PublicMethod,
    ConceptRequirement,
    ConformanceMethod,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct SyntaxNesting(usize);

impl SyntaxNesting {
    const ROOT: Self = Self(0);

    const fn nested(self) -> Option<Self> {
        if self.0 < MAX_SYNTAX_NESTING {
            Some(Self(self.0 + 1))
        } else {
            None
        }
    }
}

impl CallableContext {
    const fn is_method(self) -> bool {
        matches!(
            self,
            Self::PrivateMethod
                | Self::PublicMethod
                | Self::ConceptRequirement
                | Self::ConformanceMethod
        )
    }
}

struct Parser<'a> {
    file: FileId,
    tokens: &'a [Token],
    pos: usize,
    last_end: u32,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> Parser<'a> {
    fn new(file: FileId, tokens: &'a [Token]) -> Self {
        Self {
            file,
            tokens,
            pos: 0,
            last_end: 0,
            diagnostics: Vec::new(),
        }
    }

    fn parse_file(&mut self, source_len: usize) -> SourceFile {
        self.skip_separators();
        let mut imports = Vec::new();
        let mut declarations = Vec::new();
        let mut saw_declaration = false;

        loop {
            self.skip_separators();
            if self.at(TokenKind::Eof) {
                break;
            }

            if self.at(TokenKind::ImportKw) {
                let import = self.parse_import();
                if saw_declaration {
                    self.error_at(
                        "UnexpectedToken",
                        "all imports must appear before the first top-level declaration",
                        import.range,
                    );
                }
                imports.push(import);
                self.require_boundary("import declaration");
                continue;
            }

            if self.is_top_start() {
                saw_declaration = true;
                declarations.push(self.parse_declaration());
                continue;
            }

            let start = self.current_range().start;
            self.error_here(
                "UnexpectedToken",
                "expected a complete top-level declaration start",
            );
            self.recover_to_top_level();
            let end = self.current_range().start.max(start);
            declarations.push(Decl {
                visibility: Visibility::Private,
                kind: DeclKind::Error(ErrorNode {
                    range: TextRange::new(start, end),
                }),
                range: TextRange::new(start, end),
            });
        }

        SourceFile {
            imports,
            declarations,
            range: TextRange::new(0, to_u32(source_len)),
        }
    }

    fn parse_import(&mut self) -> ImportDecl {
        let start = self.start();
        self.expect(TokenKind::ImportKw, "`import`");
        let path = self.parse_path();
        ImportDecl {
            path,
            range: self.finish(start),
        }
    }

    fn parse_declaration(&mut self) -> Decl {
        let start = self.start();
        let visibility = if self.eat(TokenKind::PubKw).is_some() {
            Visibility::Public
        } else {
            Visibility::Private
        };

        let kind = match self.kind() {
            TokenKind::TypeKw => DeclKind::ConstrainedType(self.parse_constrained_type()),
            TokenKind::RecordKw => DeclKind::Record(self.parse_record()),
            TokenKind::EnumKw => DeclKind::Enum(self.parse_enum()),
            TokenKind::FnKw => {
                let context = if visibility.is_public() {
                    CallableContext::PublicFunction
                } else {
                    CallableContext::PrivateFunction
                };
                DeclKind::Function(self.parse_function(context, false, false))
            }
            TokenKind::AsyncKw => {
                self.bump();
                self.expect(TokenKind::FnKw, "`fn` after `async`");
                let context = if visibility.is_public() {
                    CallableContext::PublicFunction
                } else {
                    CallableContext::PrivateFunction
                };
                DeclKind::Function(self.parse_function_after_fn(context, false, true))
            }
            TokenKind::ImplKw if !visibility.is_public() => DeclKind::Impl(self.parse_impl()),
            TokenKind::TestKw if !visibility.is_public() => {
                self.bump();
                let is_async = self.eat(TokenKind::AsyncKw).is_some();
                self.expect(TokenKind::FnKw, "`fn` after `test`");
                DeclKind::Function(self.parse_function_after_fn(
                    CallableContext::Test,
                    true,
                    is_async,
                ))
            }
            TokenKind::DynKw => {
                self.bump();
                self.expect(TokenKind::ConceptKw, "`concept` after `dyn`");
                DeclKind::Concept(self.parse_concept_after_keyword(true))
            }
            TokenKind::ConceptKw => {
                self.bump();
                DeclKind::Concept(self.parse_concept_after_keyword(false))
            }
            _ => {
                self.error_here(
                    "UnexpectedToken",
                    "`pub` must be followed by `type`, `record`, `enum`, `fn`, or `(dyn) concept`",
                );
                self.recover_to_top_level();
                DeclKind::Error(ErrorNode {
                    range: self.finish(start),
                })
            }
        };
        Decl {
            visibility,
            kind,
            range: self.finish(start),
        }
    }

    fn parse_constrained_type(&mut self) -> ConstrainedTypeDecl {
        self.expect(TokenKind::TypeKw, "`type`");
        let name = self.parse_ident("constrained type name");
        self.expect(TokenKind::Eq, "`=` after constrained type name");
        let base = self.parse_type();
        let predicate = if self.eat(TokenKind::WhereKw).is_some() {
            self.parse_expr(true)
        } else {
            self.error_here(
                "UnexpectedToken",
                "a `type` declaration must include `where` and a predicate",
            );
            self.error_expr()
        };
        ConstrainedTypeDecl {
            name,
            base,
            predicate,
        }
    }

    fn parse_record(&mut self) -> RecordDecl {
        self.expect(TokenKind::RecordKw, "`record`");
        let name = self.parse_ident("record name");
        let generics = self.parse_generic_params();
        self.expect(TokenKind::LBrace, "`{` after record name");
        self.skip_separators();
        let mut fields = Vec::new();
        let mut invariant = None;

        while !self.at_any(&[TokenKind::RBrace, TokenKind::Eof]) {
            let before = self.pos;
            if self.eat(TokenKind::InvariantKw).is_some() {
                let clause_start = self.last_start();
                let predicate = self.parse_expr(true);
                if invariant.is_some() {
                    self.error_at(
                        "UnexpectedToken",
                        "a record may contain only one `invariant` clause",
                        TextRange::new(clause_start, predicate.range.end),
                    );
                } else {
                    invariant = Some(predicate);
                }
            } else if self.at(TokenKind::Ident) {
                let field_start = self.start();
                if invariant.is_some() {
                    self.error_here(
                        "UnexpectedToken",
                        "record fields must appear before the unique `invariant` clause",
                    );
                }
                let field_name = self.parse_ident("field name");
                let ty = self.parse_type();
                fields.push(RecordField {
                    name: field_name,
                    ty,
                    range: self.finish(field_start),
                });
            } else {
                self.error_here(
                    "UnexpectedToken",
                    "expected a field or the `invariant` clause",
                );
                self.recover_to_list_boundary();
            }
            self.finish_braced_item("record field or invariant");
            self.ensure_progress(before);
        }
        self.expect(TokenKind::RBrace, "`}` to close record");
        RecordDecl {
            name,
            generics,
            fields,
            invariant,
        }
    }

    fn parse_enum(&mut self) -> EnumDecl {
        self.expect(TokenKind::EnumKw, "`enum`");
        let name = self.parse_ident("enum name");
        let generics = self.parse_generic_params();
        self.expect(TokenKind::LBrace, "`{` after enum name");
        self.skip_separators();
        let mut variants = Vec::new();
        while !self.at_any(&[TokenKind::RBrace, TokenKind::Eof]) {
            let before = self.pos;
            if self.at(TokenKind::Ident) {
                let start = self.start();
                let variant_name = self.parse_ident("variant name");
                let mut payload = Vec::new();
                if self.eat(TokenKind::LParen).is_some() {
                    if !self.at(TokenKind::RParen) {
                        loop {
                            payload.push(self.parse_type());
                            if self.eat(TokenKind::Comma).is_none() {
                                break;
                            }
                        }
                    }
                    self.expect(TokenKind::RParen, "`)` after variant payload");
                }
                variants.push(EnumVariant {
                    name: variant_name,
                    payload,
                    range: self.finish(start),
                });
            } else {
                self.error_here("UnexpectedToken", "expected an enum variant");
                self.recover_to_list_boundary();
            }
            self.finish_braced_item("enum variant");
            self.ensure_progress(before);
        }
        self.expect(TokenKind::RBrace, "`}` to close enum");
        EnumDecl {
            name,
            generics,
            variants,
        }
    }

    fn parse_function(
        &mut self,
        context: CallableContext,
        is_test: bool,
        is_async: bool,
    ) -> FunctionDecl {
        self.expect(TokenKind::FnKw, "`fn`");
        self.parse_function_after_fn(context, is_test, is_async)
    }

    fn parse_function_after_fn(
        &mut self,
        context: CallableContext,
        is_test: bool,
        is_async: bool,
    ) -> FunctionDecl {
        let signature = self.parse_signature(context, false);
        if is_test {
            if !signature.generics.is_empty() {
                self.error_at(
                    "UnexpectedToken",
                    "a `test fn` cannot declare generic parameters",
                    signature.range,
                );
            }
            if !signature.parameters.is_empty() {
                self.error_at(
                    "UnexpectedToken",
                    "a `test fn` must have an empty parameter list",
                    signature.range,
                );
            }
        }
        self.skip_separators();
        let body = self.parse_block();
        self.reject_bare_unit_tail(&body);
        FunctionDecl {
            signature,
            body,
            is_test,
            is_async,
        }
    }

    fn parse_signature(&mut self, context: CallableContext, is_static: bool) -> CallableSignature {
        let start = self.start();
        let name = self.parse_ident("callable name");
        let generics = self.parse_generic_params();
        self.expect(TokenKind::LParen, "`(` after callable name");
        let (receiver, parameters) = self.parse_parameters(context.is_method() && !is_static);
        self.expect(TokenKind::RParen, "`)` after parameters");

        let had_separator = self.at(TokenKind::Separator);
        if had_separator {
            self.skip_separators();
        }
        let return_type = if self.starts_type()
            && !self.is_top_start()
            && !self.at_any(&[
                TokenKind::RequiresKw,
                TokenKind::EnsuresKw,
                TokenKind::LBrace,
            ]) {
            Some(self.parse_type())
        } else {
            None
        };
        if let Some(return_type) = &return_type
            && is_bare_unit_type(return_type)
        {
            self.error_at(
                "UnexpectedToken",
                "`Unit` return types are implicit; omit `Unit` after the parameter list",
                return_type.range,
            );
        }

        self.skip_separators();
        let contracts = self.parse_contracts();
        CallableSignature {
            name,
            generics,
            receiver,
            parameters,
            return_type,
            contracts,
            range: self.finish(start),
        }
    }

    fn parse_parameters(&mut self, allow_receiver: bool) -> (Option<Receiver>, Vec<Parameter>) {
        let mut receiver = None;
        let mut parameters = Vec::new();
        if self.at(TokenKind::RParen) {
            return (receiver, parameters);
        }

        if allow_receiver && self.at(TokenKind::SelfValueKw) {
            let token = self.bump();
            receiver = Some(Receiver::ReadOnly(token.range));
            if self.eat(TokenKind::Comma).is_none() {
                return (receiver, parameters);
            }
        } else if allow_receiver
            && self.at(TokenKind::MutKw)
            && self.nth_kind(1) == TokenKind::SelfValueKw
        {
            let start = self.start();
            self.bump();
            self.bump();
            receiver = Some(Receiver::Mutable(self.finish(start)));
            if self.eat(TokenKind::Comma).is_none() {
                return (receiver, parameters);
            }
        }

        while !self.at_any(&[TokenKind::RParen, TokenKind::Eof]) {
            if self.is_declaration_sync_start() && self.has_physical_line_break_before_current() {
                self.error_here(
                    "UnexpectedToken",
                    "the parameter list is missing `)` before this top-level declaration",
                );
                break;
            }
            let start = self.start();
            let name = self.parse_ident("parameter name");
            let ty = self.parse_type();
            parameters.push(Parameter {
                name,
                ty,
                range: self.finish(start),
            });
            if self.eat(TokenKind::Comma).is_none() {
                break;
            }
        }
        (receiver, parameters)
    }

    fn parse_contracts(&mut self) -> Vec<Contract> {
        let mut contracts = Vec::new();
        let mut saw_ensures = false;
        while self.at_any(&[TokenKind::RequiresKw, TokenKind::EnsuresKw]) {
            let start = self.start();
            let kind = if self.eat(TokenKind::RequiresKw).is_some() {
                if saw_ensures {
                    self.error_at(
                        "UnexpectedToken",
                        "all `requires` clauses must precede every `ensures` clause",
                        TextRange::new(start, self.last_end),
                    );
                }
                ContractKind::Requires
            } else {
                self.bump();
                saw_ensures = true;
                ContractKind::Ensures
            };
            let predicate = self.parse_expr(true);
            contracts.push(Contract {
                kind,
                range: self.finish(start),
                predicate,
            });
            self.skip_separators();
        }
        contracts
    }

    fn parse_impl(&mut self) -> ImplDecl {
        self.expect(TokenKind::ImplKw, "`impl`");
        let generics = self.parse_generic_params();
        let first = self.parse_type();
        if self.eat(TokenKind::ForKw).is_some() {
            let concept = self.type_as_concept(first);
            let target = self.parse_type();
            self.expect(TokenKind::LBrace, "`{` after conformance head");
            let members = self.parse_conformance_members();
            self.expect(TokenKind::RBrace, "`}` to close conformance");
            ImplDecl {
                generics,
                kind: ImplKind::Conformance {
                    concept,
                    target,
                    members,
                },
            }
        } else {
            self.expect(TokenKind::LBrace, "`{` after inherent impl target");
            let methods = self.parse_inherent_methods();
            self.expect(TokenKind::RBrace, "`}` to close inherent impl");
            ImplDecl {
                generics,
                kind: ImplKind::Inherent {
                    target: first,
                    methods,
                },
            }
        }
    }

    fn parse_inherent_methods(&mut self) -> Vec<MethodDecl> {
        let mut methods = Vec::new();
        self.skip_separators();
        while !self.at_any(&[TokenKind::RBrace, TokenKind::Eof]) {
            if self.is_top_start() {
                self.error_here(
                    "UnexpectedToken",
                    "the impl is missing `}` before this top-level declaration",
                );
                break;
            }
            let before = self.pos;
            let visibility = if self.eat(TokenKind::PubKw).is_some() {
                Visibility::Public
            } else {
                Visibility::Private
            };
            if self.eat(TokenKind::MethodKw).is_some() {
                let context = if visibility.is_public() {
                    CallableContext::PublicMethod
                } else {
                    CallableContext::PrivateMethod
                };
                methods.push(self.parse_method_after_keyword(visibility, false, context));
            } else {
                self.error_here(
                    "UnexpectedToken",
                    "an inherent impl may contain only `(pub)? method` declarations",
                );
                self.recover_to_impl_member();
            }
            self.skip_separators();
            self.ensure_progress(before);
        }
        methods
    }

    fn parse_conformance_members(&mut self) -> Vec<ConformanceMember> {
        let mut members = Vec::new();
        self.skip_separators();
        while !self.at_any(&[TokenKind::RBrace, TokenKind::Eof]) {
            if self.is_top_start() {
                self.error_here(
                    "UnexpectedToken",
                    "the conformance is missing `}` before this top-level declaration",
                );
                break;
            }
            let before = self.pos;
            if self.at(TokenKind::AssociatedKw) {
                members.push(ConformanceMember::AssociatedType(
                    self.parse_associated_binding(),
                ));
            } else {
                let is_static = self.eat(TokenKind::StaticKw).is_some();
                if self.eat(TokenKind::MethodKw).is_some() {
                    let method = self.parse_method_after_keyword(
                        Visibility::Private,
                        is_static,
                        CallableContext::ConformanceMethod,
                    );
                    if !method.signature.contracts.is_empty() {
                        self.error_at(
                            "UnexpectedToken",
                            "a conformance inherits its concept contract and cannot redeclare it",
                            method.signature.range,
                        );
                    }
                    members.push(ConformanceMember::Method(method));
                } else {
                    self.error_here(
                        "UnexpectedToken",
                        "expected `associated type`, `method`, or `static method`",
                    );
                    let start = self.start();
                    self.recover_to_conformance_member();
                    members.push(ConformanceMember::Error(ErrorNode {
                        range: self.finish(start),
                    }));
                }
            }
            self.skip_separators();
            self.ensure_progress(before);
        }
        members
    }

    fn parse_method_after_keyword(
        &mut self,
        visibility: Visibility,
        is_static: bool,
        context: CallableContext,
    ) -> MethodDecl {
        let start = self.start();
        let signature = self.parse_signature(context, is_static);
        self.skip_separators();
        let body = self.parse_block();
        self.reject_bare_unit_tail(&body);
        MethodDecl {
            visibility,
            is_static,
            signature,
            body,
            range: self.finish(start),
        }
    }

    fn parse_associated_binding(&mut self) -> AssociatedTypeBinding {
        let start = self.start();
        self.expect(TokenKind::AssociatedKw, "`associated`");
        self.expect(TokenKind::TypeKw, "`type` after `associated`");
        let name = self.parse_ident("associated type name");
        self.expect(TokenKind::Eq, "`=` in associated type binding");
        let value = self.parse_type();
        AssociatedTypeBinding {
            name,
            value,
            range: self.finish(start),
        }
    }

    fn parse_concept_after_keyword(&mut self, dynamic: bool) -> ConceptDecl {
        let name = self.parse_ident("concept name");
        if self.at(TokenKind::LBracket) {
            self.error_here(
                "UnexpectedToken",
                "concept declarations do not have type parameters",
            );
            let _ = self.parse_generic_params();
        }
        self.expect(TokenKind::LBrace, "`{` after concept name");
        self.skip_separators();
        let mut members = Vec::new();
        while !self.at_any(&[TokenKind::RBrace, TokenKind::Eof]) {
            if self.is_top_start() {
                self.error_here(
                    "UnexpectedToken",
                    "the concept is missing `}` before this top-level declaration",
                );
                break;
            }
            let before = self.pos;
            if self.at(TokenKind::AssociatedKw) {
                members.push(ConceptMember::AssociatedType(
                    self.parse_associated_requirement(),
                ));
            } else {
                let is_static = self.eat(TokenKind::StaticKw).is_some();
                if self.eat(TokenKind::MethodKw).is_some() {
                    let start = self.start();
                    let signature =
                        self.parse_signature(CallableContext::ConceptRequirement, is_static);
                    if self.at(TokenKind::LBrace) {
                        self.error_here(
                            "UnexpectedToken",
                            "concept requirements cannot provide a default method body",
                        );
                        let _ = self.parse_block();
                    }
                    members.push(ConceptMember::Method(MethodRequirement {
                        is_static,
                        range: self.finish(start),
                        signature,
                    }));
                } else {
                    self.error_here(
                        "UnexpectedToken",
                        "expected `associated type`, `method`, or `static method`",
                    );
                    let start = self.start();
                    self.recover_to_concept_member();
                    members.push(ConceptMember::Error(ErrorNode {
                        range: self.finish(start),
                    }));
                }
            }
            self.skip_separators();
            self.ensure_progress(before);
        }
        self.expect(TokenKind::RBrace, "`}` to close concept");
        ConceptDecl {
            name,
            dynamic,
            members,
        }
    }

    fn parse_associated_requirement(&mut self) -> AssociatedTypeRequirement {
        let start = self.start();
        self.expect(TokenKind::AssociatedKw, "`associated`");
        self.expect(TokenKind::TypeKw, "`type` after `associated`");
        let name = self.parse_ident("associated type name");
        let bounds = if self.eat(TokenKind::Colon).is_some() {
            self.parse_bounds()
        } else {
            Vec::new()
        };
        AssociatedTypeRequirement {
            name,
            bounds,
            range: self.finish(start),
        }
    }

    fn parse_generic_params(&mut self) -> Vec<GenericParam> {
        if self.eat(TokenKind::LBracket).is_none() {
            return Vec::new();
        }
        let mut params = Vec::new();
        if !self.at(TokenKind::RBracket) {
            loop {
                if self.is_declaration_sync_start() && self.has_physical_line_break_before_current()
                {
                    self.error_here(
                        "UnexpectedToken",
                        "the generic parameter list is missing `]` before this top-level declaration",
                    );
                    break;
                }
                let start = self.start();
                let name = self.parse_ident("generic parameter name");
                let bounds = if self.eat(TokenKind::Colon).is_some() {
                    self.parse_bounds()
                } else {
                    Vec::new()
                };
                params.push(GenericParam {
                    name,
                    bounds,
                    range: self.finish(start),
                });
                if self.eat(TokenKind::Comma).is_none() {
                    break;
                }
            }
        }
        self.expect(TokenKind::RBracket, "`]` after generic parameters");
        params
    }

    fn parse_bounds(&mut self) -> Vec<ConceptRef> {
        let mut bounds = vec![self.parse_concept_ref()];
        while self.eat(TokenKind::Plus).is_some() {
            bounds.push(self.parse_concept_ref());
        }
        bounds
    }

    fn parse_type(&mut self) -> TypeExpr {
        self.parse_type_at(SyntaxNesting::ROOT)
    }

    fn parse_type_at(&mut self, nesting: SyntaxNesting) -> TypeExpr {
        let start = self.start();
        match self.kind() {
            TokenKind::LParen => {
                let Some(child_nesting) = self.enter_nesting(nesting, "tuple type") else {
                    return self.error_type_from(start);
                };
                self.bump();
                let first = self.parse_type_at(child_nesting);
                if self.eat(TokenKind::Comma).is_none() {
                    self.expect(TokenKind::RParen, "`)` after grouped type");
                    return TypeExpr {
                        range: self.finish(start),
                        ..first
                    };
                }
                let mut elements = vec![first];
                while !self.at_any(&[TokenKind::RParen, TokenKind::Eof]) {
                    elements.push(self.parse_type_at(child_nesting));
                    if self.eat(TokenKind::Comma).is_none() {
                        break;
                    }
                }
                self.expect(TokenKind::RParen, "`)` after tuple type");
                TypeExpr {
                    kind: TypeExprKind::Tuple(elements),
                    range: self.finish(start),
                }
            }
            TokenKind::DynKw => {
                let Some(child_nesting) = self.enter_nesting(nesting, "dyn type") else {
                    return self.error_type_from(start);
                };
                self.bump();
                let target = self.parse_concept_ref_at(child_nesting);
                TypeExpr {
                    kind: TypeExprKind::BareDyn(target),
                    range: self.finish(start),
                }
            }
            TokenKind::Lt => self.parse_qualified_projection_at(nesting),
            TokenKind::Ident => {
                let path = self.parse_path();
                let arguments = if self.at(TokenKind::LBracket) {
                    let Some(child_nesting) = self.enter_nesting(nesting, "type arguments") else {
                        return self.error_type_from(start);
                    };
                    self.parse_type_arguments_at(child_nesting)
                } else {
                    Vec::new()
                };
                TypeExpr {
                    kind: TypeExprKind::Named { path, arguments },
                    range: self.finish(start),
                }
            }
            _ => {
                self.error_here("UnexpectedToken", "expected a type expression");
                if !self.at_any(&[
                    TokenKind::Separator,
                    TokenKind::Comma,
                    TokenKind::RParen,
                    TokenKind::RBracket,
                    TokenKind::RBrace,
                    TokenKind::Eof,
                ]) {
                    self.bump();
                }
                TypeExpr {
                    kind: TypeExprKind::Error,
                    range: self.finish(start),
                }
            }
        }
    }

    fn parse_qualified_projection_at(&mut self, nesting: SyntaxNesting) -> TypeExpr {
        let start = self.start();
        let Some(child_nesting) = self.enter_nesting(nesting, "qualified type projection") else {
            return self.error_type_from(start);
        };
        self.expect(TokenKind::Lt, "`<`");
        let base = self.parse_type_at(child_nesting);
        self.expect(TokenKind::AsKw, "`as` in qualified associated projection");
        let concept = self.parse_concept_ref_at(child_nesting);
        self.expect(TokenKind::Gt, "`>` in qualified associated projection");
        self.expect(TokenKind::Dot, "`.` before associated type name");
        let associated = self.parse_ident("associated type name");
        TypeExpr {
            kind: TypeExprKind::QualifiedProjection {
                base: Box::new(base),
                concept,
                associated,
            },
            range: self.finish(start),
        }
    }

    fn parse_type_arguments_at(&mut self, nesting: SyntaxNesting) -> Vec<TypeArgument> {
        self.expect(TokenKind::LBracket, "`[`");
        let mut arguments = Vec::new();
        if !self.at(TokenKind::RBracket) {
            loop {
                if self.is_declaration_sync_start() && self.has_physical_line_break_before_current()
                {
                    self.error_here(
                        "UnexpectedToken",
                        "the type argument list is missing `]` before this top-level declaration",
                    );
                    break;
                }
                if self.at(TokenKind::Ident) && self.nth_kind(1) == TokenKind::Eq {
                    let start = self.start();
                    let name = self.parse_ident("associated binding name");
                    self.bump();
                    let ty = self.parse_type_at(nesting);
                    arguments.push(TypeArgument::Binding(AssociatedBinding {
                        name,
                        ty,
                        range: self.finish(start),
                    }));
                } else {
                    arguments.push(TypeArgument::Type(self.parse_type_at(nesting)));
                }
                if self.eat(TokenKind::Comma).is_none() {
                    break;
                }
            }
        }
        self.expect(TokenKind::RBracket, "`]` after type arguments");
        arguments
    }

    fn parse_concept_ref(&mut self) -> ConceptRef {
        self.parse_concept_ref_at(SyntaxNesting::ROOT)
    }

    fn parse_concept_ref_at(&mut self, nesting: SyntaxNesting) -> ConceptRef {
        let start = self.start();
        let path = self.parse_path();
        let mut bindings = Vec::new();
        if self.at(TokenKind::LBracket) {
            let Some(child_nesting) = self.enter_nesting(nesting, "associated type bindings")
            else {
                return ConceptRef {
                    path,
                    bindings,
                    range: self.finish(start),
                };
            };
            self.bump();
            if !self.at(TokenKind::RBracket) {
                loop {
                    if self.is_declaration_sync_start()
                        && self.has_physical_line_break_before_current()
                    {
                        self.error_here(
                            "UnexpectedToken",
                            "the associated binding list is missing `]` before this top-level declaration",
                        );
                        break;
                    }
                    let binding_start = self.start();
                    let name = self.parse_ident("associated binding name");
                    self.expect(TokenKind::Eq, "`=` in associated binding");
                    let ty = self.parse_type_at(child_nesting);
                    bindings.push(AssociatedBinding {
                        name,
                        ty,
                        range: self.finish(binding_start),
                    });
                    if self.eat(TokenKind::Comma).is_none() {
                        break;
                    }
                }
            }
            self.expect(TokenKind::RBracket, "`]` after associated bindings");
        }
        ConceptRef {
            path,
            bindings,
            range: self.finish(start),
        }
    }

    fn type_as_concept(&mut self, ty: TypeExpr) -> ConceptRef {
        let range = ty.range;
        if let TypeExprKind::Named { path, arguments } = ty.kind {
            let mut bindings = Vec::new();
            for argument in arguments {
                match argument {
                    TypeArgument::Binding(binding) => bindings.push(binding),
                    TypeArgument::Type(argument) => self.error_at(
                        "UnexpectedToken",
                        "concept references may bind associated types but do not take positional type arguments",
                        argument.range,
                    ),
                }
            }
            ConceptRef {
                path,
                bindings,
                range,
            }
        } else {
            self.error_at(
                "UnexpectedToken",
                "the left side of `for` must be a concept reference",
                range,
            );
            ConceptRef {
                path: Path {
                    segments: Vec::new(),
                    range,
                },
                bindings: Vec::new(),
                range,
            }
        }
    }

    fn parse_expr(&mut self, allow_record_literal: bool) -> Expr {
        self.parse_expr_at(allow_record_literal, SyntaxNesting::ROOT)
    }

    fn parse_expr_at(&mut self, allow_record_literal: bool, nesting: SyntaxNesting) -> Expr {
        self.parse_binary_at(0, allow_record_literal, nesting)
    }

    fn parse_binary_at(
        &mut self,
        minimum: u8,
        allow_record_literal: bool,
        nesting: SyntaxNesting,
    ) -> Expr {
        let mut left = self.parse_unary_at(allow_record_literal, nesting);
        let mut chain_nesting = nesting;
        while let Some((op, precedence)) = self.current_binary_op() {
            if precedence < minimum {
                break;
            }
            let Some(child_nesting) = self.enter_nesting(chain_nesting, "binary expression") else {
                break;
            };
            let operator_range = self.current_range();
            self.bump();
            let right = self.parse_binary_at(precedence + 1, allow_record_literal, child_nesting);
            if op.is_comparison()
                && matches!(
                    left.kind,
                    ExprKind::Binary {
                        op: previous,
                        ..
                    } if previous.is_comparison()
                )
            {
                self.error_at(
                    "ChainedComparison",
                    "comparison operators are non-associative; combine comparisons explicitly with `&&` or `||`",
                    operator_range,
                );
            }
            let range = TextRange::new(left.range.start, right.range.end);
            left = Expr {
                kind: ExprKind::Binary {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                },
                range,
            };
            chain_nesting = child_nesting;
        }
        left
    }

    fn parse_unary_at(&mut self, allow_record_literal: bool, nesting: SyntaxNesting) -> Expr {
        if matches!(self.kind(), TokenKind::Minus | TokenKind::Bang) {
            self.parse_prefix_chain_at(allow_record_literal, nesting)
        } else {
            self.parse_postfix_at(allow_record_literal, nesting)
        }
    }

    fn parse_prefix_chain_at(
        &mut self,
        allow_record_literal: bool,
        nesting: SyntaxNesting,
    ) -> Expr {
        struct Prefix(u32, UnaryOp);

        let mut prefixes = Vec::new();
        let mut chain_nesting = nesting;
        loop {
            let start = self.start();
            let op = match self.kind() {
                TokenKind::Minus => Some(UnaryOp::Negate),
                TokenKind::Bang => Some(UnaryOp::Not),
                _ => None,
            };
            let Some(op) = op else {
                break;
            };
            let Some(child_nesting) = self.enter_nesting(chain_nesting, "unary expression") else {
                return self.error_expr_from(start);
            };
            self.bump();
            prefixes.push(Prefix(start, op));
            chain_nesting = child_nesting;
        }

        let mut expression = self.parse_postfix_at(allow_record_literal, chain_nesting);
        for Prefix(start, op) in prefixes.into_iter().rev() {
            expression = Expr {
                range: TextRange::new(start, expression.range.end),
                kind: ExprKind::Unary {
                    op,
                    operand: Box::new(expression),
                },
            };
        }
        expression
    }

    fn parse_postfix_at(&mut self, allow_record_literal: bool, nesting: SyntaxNesting) -> Expr {
        let mut expr = self.parse_primary_at(allow_record_literal, nesting);
        let mut chain_nesting = nesting;
        loop {
            if self.at(TokenKind::LBracket) {
                let Some(child_nesting) =
                    self.enter_nesting(chain_nesting, "generic call expression")
                else {
                    break;
                };
                let type_arguments = self.parse_call_type_arguments_at(child_nesting);
                if self.at(TokenKind::LParen) {
                    expr = self.parse_call_at(expr, type_arguments, child_nesting);
                    chain_nesting = child_nesting;
                } else {
                    self.error_here(
                        "UnexpectedToken",
                        "expression type arguments must be followed by a call",
                    );
                }
                continue;
            }
            if self.at(TokenKind::LParen) {
                let Some(child_nesting) = self.enter_nesting(chain_nesting, "call expression")
                else {
                    break;
                };
                expr = self.parse_call_at(expr, Vec::new(), child_nesting);
                chain_nesting = child_nesting;
                continue;
            }
            if self.at(TokenKind::Dot) {
                let Some(child_nesting) = self.enter_nesting(chain_nesting, "member projection")
                else {
                    break;
                };
                expr = self.parse_dot_postfix(expr);
                chain_nesting = child_nesting;
                continue;
            }
            if self.at(TokenKind::Question) {
                let Some(child_nesting) = self.enter_nesting(chain_nesting, "result propagation")
                else {
                    break;
                };
                expr = self.parse_propagate_postfix(expr);
                chain_nesting = child_nesting;
                continue;
            }
            if allow_record_literal
                && self.at(TokenKind::LBrace)
                && let Some(path) = expr_as_path(&expr)
            {
                let Some(child_nesting) = self.enter_nesting(chain_nesting, "record literal")
                else {
                    break;
                };
                expr = self.parse_record_literal_at(path, expr.range.start, child_nesting);
                chain_nesting = child_nesting;
                continue;
            }
            break;
        }
        expr
    }

    fn parse_dot_postfix(&mut self, receiver: Expr) -> Expr {
        self.bump();
        if self.at(TokenKind::AwaitKw) {
            let await_token = self.bump();
            return Expr {
                range: TextRange::new(receiver.range.start, await_token.range.end),
                kind: ExprKind::Await(Box::new(receiver)),
            };
        }
        let name = self.parse_ident("member name after `.`");
        let range = TextRange::new(receiver.range.start, name.range.end);
        Expr {
            kind: ExprKind::Member {
                receiver: Box::new(receiver),
                name,
            },
            range,
        }
    }

    fn parse_propagate_postfix(&mut self, value: Expr) -> Expr {
        let question = self.bump();
        Expr {
            range: TextRange::new(value.range.start, question.range.end),
            kind: ExprKind::Propagate(Box::new(value)),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn parse_primary_at(&mut self, _allow_record_literal: bool, nesting: SyntaxNesting) -> Expr {
        if let Some(literal) = self.parse_literal_expr() {
            return literal;
        }
        let start = self.start();
        match self.kind() {
            TokenKind::Ident => {
                let ident = self.parse_ident("name");
                let path = Path {
                    range: ident.range,
                    segments: vec![ident],
                };
                Expr {
                    kind: ExprKind::Name(path),
                    range: self.finish(start),
                }
            }
            TokenKind::SelfValueKw => {
                let token = self.bump();
                Expr {
                    kind: ExprKind::SelfValue,
                    range: token.range,
                }
            }
            TokenKind::ResultKw => {
                let token = self.bump();
                Expr {
                    kind: ExprKind::ContractResult,
                    range: token.range,
                }
            }
            TokenKind::OldKw => self.parse_old_at(nesting),
            TokenKind::LParen => {
                let Some(child_nesting) = self.enter_nesting(nesting, "parenthesized expression")
                else {
                    return self.error_expr_from(start);
                };
                self.bump();
                let first = self.parse_expr_at(true, child_nesting);
                if self.eat(TokenKind::Comma).is_none() {
                    self.expect(TokenKind::RParen, "`)` after parenthesized expression");
                    return Expr {
                        range: self.finish(start),
                        ..first
                    };
                }
                let mut elements = vec![first];
                while !self.at_any(&[TokenKind::RParen, TokenKind::Eof]) {
                    elements.push(self.parse_expr_at(true, child_nesting));
                    if self.eat(TokenKind::Comma).is_none() {
                        break;
                    }
                }
                self.expect(TokenKind::RParen, "`)` after tuple expression");
                Expr {
                    kind: ExprKind::Tuple(elements),
                    range: self.finish(start),
                }
            }
            TokenKind::LBracket => {
                let Some(child_nesting) = self.enter_nesting(nesting, "list expression") else {
                    return self.error_expr_from(start);
                };
                self.bump();
                let mut elements = Vec::new();
                while !self.at_any(&[TokenKind::RBracket, TokenKind::Eof]) {
                    elements.push(self.parse_expr_at(true, child_nesting));
                    if self.eat(TokenKind::Comma).is_none() {
                        break;
                    }
                }
                self.expect(TokenKind::RBracket, "`]` after list expression");
                Expr {
                    kind: ExprKind::List(elements),
                    range: self.finish(start),
                }
            }
            TokenKind::LBrace => Expr {
                kind: ExprKind::Block(self.parse_block_at(nesting)),
                range: self.finish(start),
            },
            TokenKind::IfKw => self.parse_if_at(nesting),
            TokenKind::MatchKw => self.parse_match_at(nesting),
            TokenKind::Lt => self.parse_qualified_member_at(nesting),
            _ => {
                self.error_here("UnexpectedToken", "expected an expression");
                if !self.at_expr_boundary() {
                    self.bump();
                }
                self.error_expr_from(start)
            }
        }
    }

    fn parse_literal_expr(&mut self) -> Option<Expr> {
        let kind = match self.kind() {
            TokenKind::IntLiteral => Literal::Int(self.current_token().text.clone()),
            TokenKind::FloatLiteral => Literal::Float(self.current_token().text.clone()),
            TokenKind::TextLiteral => Literal::Text(self.current_token().text.clone()),
            TokenKind::TrueKw => Literal::Bool(true),
            TokenKind::FalseKw => Literal::Bool(false),
            _ => return None,
        };
        let token = self.bump();
        Some(Expr {
            kind: ExprKind::Literal(kind),
            range: token.range,
        })
    }

    fn parse_old_at(&mut self, nesting: SyntaxNesting) -> Expr {
        let start = self.start();
        let Some(child_nesting) = self.enter_nesting(nesting, "old expression") else {
            return self.error_expr_from(start);
        };
        self.bump();
        self.expect(TokenKind::LParen, "`(` after `old`");
        let value = self.parse_expr_at(true, child_nesting);
        self.expect(TokenKind::RParen, "`)` after `old` expression");
        Expr {
            kind: ExprKind::Old(Box::new(value)),
            range: self.finish(start),
        }
    }

    fn parse_qualified_member_at(&mut self, nesting: SyntaxNesting) -> Expr {
        let start = self.start();
        let Some(child_nesting) = self.enter_nesting(nesting, "qualified member projection") else {
            return self.error_expr_from(start);
        };
        self.bump();
        let base = self.parse_type_at(child_nesting);
        self.expect(TokenKind::AsKw, "`as` in qualified method selection");
        let concept = self.parse_concept_ref_at(child_nesting);
        self.expect(TokenKind::Gt, "`>` in qualified method selection");
        self.expect(TokenKind::Dot, "`.` before qualified method name");
        let name = self.parse_ident("qualified method name");
        Expr {
            kind: ExprKind::QualifiedMember {
                base,
                concept,
                name,
            },
            range: self.finish(start),
        }
    }

    fn parse_call_type_arguments_at(&mut self, nesting: SyntaxNesting) -> Vec<TypeExpr> {
        self.expect(TokenKind::LBracket, "`[`");
        let mut arguments = Vec::new();
        if !self.at(TokenKind::RBracket) {
            loop {
                if self.is_declaration_sync_start() && self.has_physical_line_break_before_current()
                {
                    self.error_here(
                        "UnexpectedToken",
                        "the call type argument list is missing `]` before this top-level declaration",
                    );
                    break;
                }
                arguments.push(self.parse_type_at(nesting));
                if self.eat(TokenKind::Comma).is_none() {
                    break;
                }
            }
        }
        self.expect(TokenKind::RBracket, "`]` after call type arguments");
        arguments
    }

    fn parse_call_at(
        &mut self,
        callee: Expr,
        type_arguments: Vec<TypeExpr>,
        nesting: SyntaxNesting,
    ) -> Expr {
        let start = callee.range.start;
        self.expect(TokenKind::LParen, "`(`");
        let mut arguments = Vec::new();
        if !self.at(TokenKind::RParen) {
            loop {
                if self.is_declaration_sync_start() && self.has_physical_line_break_before_current()
                {
                    self.error_here(
                        "UnexpectedToken",
                        "the call argument list is missing `)` before this top-level declaration",
                    );
                    break;
                }
                arguments.push(self.parse_expr_at(true, nesting));
                if self.eat(TokenKind::Comma).is_none() {
                    break;
                }
            }
        }
        self.expect(TokenKind::RParen, "`)` after call arguments");
        Expr {
            kind: ExprKind::Call {
                callee: Box::new(callee),
                type_arguments,
                arguments,
            },
            range: self.finish(start),
        }
    }

    fn parse_record_literal_at(
        &mut self,
        constructor: Path,
        start: u32,
        nesting: SyntaxNesting,
    ) -> Expr {
        self.expect(TokenKind::LBrace, "`{` in record literal");
        self.skip_separators();
        let mut fields = Vec::new();
        while !self.at_any(&[TokenKind::RBrace, TokenKind::Eof]) {
            let before = self.pos;
            let field_start = self.start();
            let name = self.parse_ident("record literal field name");
            self.expect(TokenKind::Eq, "`=` after record literal field name");
            let value = self.parse_expr_at(true, nesting);
            fields.push(RecordLiteralField {
                name,
                value,
                range: self.finish(field_start),
            });
            self.finish_braced_item("record literal field");
            self.ensure_progress(before);
        }
        self.expect(TokenKind::RBrace, "`}` after record literal");
        Expr {
            kind: ExprKind::RecordLiteral {
                constructor,
                fields,
            },
            range: self.finish(start),
        }
    }

    fn parse_if_at(&mut self, nesting: SyntaxNesting) -> Expr {
        let start = self.start();
        let Some(child_nesting) = self.enter_nesting(nesting, "if expression") else {
            return self.error_expr_from(start);
        };
        self.bump();
        let condition = self.parse_expr_at(false, child_nesting);
        self.skip_separators();
        let then_branch = self.parse_block_at(child_nesting);
        let checkpoint = (self.pos, self.last_end);
        self.skip_separators();
        let else_branch = if self.eat(TokenKind::ElseKw).is_some() {
            self.skip_separators();
            if self.at(TokenKind::IfKw) {
                Some(ElseBranch::If(Box::new(self.parse_if_at(child_nesting))))
            } else {
                Some(ElseBranch::Block(self.parse_block_at(child_nesting)))
            }
        } else {
            (self.pos, self.last_end) = checkpoint;
            None
        };
        Expr {
            kind: ExprKind::If {
                condition: Box::new(condition),
                then_branch,
                else_branch,
            },
            range: self.finish(start),
        }
    }

    fn parse_match_at(&mut self, nesting: SyntaxNesting) -> Expr {
        let start = self.start();
        let Some(child_nesting) = self.enter_nesting(nesting, "match expression") else {
            return self.error_expr_from(start);
        };
        self.bump();
        let scrutinee = self.parse_expr_at(false, child_nesting);
        self.skip_separators();
        self.expect(TokenKind::LBrace, "`{` after match scrutinee");
        self.skip_separators();
        let mut arms = Vec::new();
        while !self.at_any(&[TokenKind::RBrace, TokenKind::Eof]) {
            let before = self.pos;
            let arm_start = self.start();
            let pattern = self.parse_pattern_at(child_nesting);
            self.expect(TokenKind::FatArrow, "`=>` after match pattern");
            let value = self.parse_expr_at(true, child_nesting);
            arms.push(MatchArm {
                pattern,
                value,
                range: self.finish(arm_start),
            });
            self.finish_braced_item("match arm");
            self.ensure_progress(before);
        }
        self.expect(TokenKind::RBrace, "`}` after match arms");
        Expr {
            kind: ExprKind::Match {
                scrutinee: Box::new(scrutinee),
                arms,
            },
            range: self.finish(start),
        }
    }

    fn parse_pattern_at(&mut self, nesting: SyntaxNesting) -> Pattern {
        let start = self.start();
        let kind = match self.kind() {
            TokenKind::Underscore => {
                self.bump();
                PatternKind::Wildcard
            }
            TokenKind::IntLiteral => PatternKind::Literal(Literal::Int(self.bump().text)),
            TokenKind::FloatLiteral => PatternKind::Literal(Literal::Float(self.bump().text)),
            TokenKind::TextLiteral => PatternKind::Literal(Literal::Text(self.bump().text)),
            TokenKind::TrueKw | TokenKind::FalseKw => {
                let value = self.kind() == TokenKind::TrueKw;
                self.bump();
                PatternKind::Literal(Literal::Bool(value))
            }
            TokenKind::Ident => {
                let path = self.parse_path();
                let mut payload = Vec::new();
                if self.at(TokenKind::LParen) {
                    let Some(child_nesting) =
                        self.enter_nesting(nesting, "constructor pattern payload")
                    else {
                        return Pattern {
                            kind: PatternKind::Error,
                            range: self.finish(start),
                        };
                    };
                    self.bump();
                    if !self.at(TokenKind::RParen) {
                        loop {
                            payload.push(self.parse_pattern_at(child_nesting));
                            if self.eat(TokenKind::Comma).is_none() {
                                break;
                            }
                        }
                    }
                    self.expect(TokenKind::RParen, "`)` after pattern payload");
                }
                PatternKind::Name { path, payload }
            }
            TokenKind::Minus => {
                self.error_here(
                    "UnexpectedToken",
                    "negative numbers are unary expressions, not literal patterns",
                );
                self.bump();
                if self.at_any(&[TokenKind::IntLiteral, TokenKind::FloatLiteral]) {
                    self.bump();
                }
                PatternKind::Error
            }
            _ => {
                self.error_here("UnexpectedToken", "expected a match pattern");
                if !self.at_any(&[TokenKind::FatArrow, TokenKind::Eof]) {
                    self.bump();
                }
                PatternKind::Error
            }
        };
        Pattern {
            kind,
            range: self.finish(start),
        }
    }

    fn parse_block(&mut self) -> Block {
        self.parse_block_at(SyntaxNesting::ROOT)
    }

    fn parse_block_at(&mut self, nesting: SyntaxNesting) -> Block {
        let start = self.start();
        if !self.at(TokenKind::LBrace) {
            self.error_here("UnexpectedToken", "expected a `{ ... }` block");
            return Block {
                items: Vec::new(),
                range: TextRange::new(start, start),
            };
        }
        let Some(child_nesting) = self.enter_nesting(nesting, "block") else {
            return Block {
                items: Vec::new(),
                range: self.finish(start),
            };
        };
        self.bump();
        self.skip_separators();
        let mut items = Vec::new();
        while !self.at_any(&[TokenKind::RBrace, TokenKind::Eof]) {
            if self.is_top_start()
                || self.is_inherent_member_start()
                || self.is_conformance_member_start()
            {
                self.error_here(
                    "UnexpectedToken",
                    "the block is missing `}` before this declaration",
                );
                break;
            }
            let before = self.pos;
            items.push(self.parse_block_item_at(child_nesting));
            if self.at(TokenKind::Separator) {
                self.skip_separators();
            } else if !self.at_any(&[TokenKind::RBrace, TokenKind::Eof]) {
                self.error_here(
                    "UnexpectedToken",
                    "block items must be separated by a newline",
                );
                self.recover_to_block_boundary();
                self.skip_separators();
            }
            self.ensure_progress(before);
        }
        self.expect(TokenKind::RBrace, "`}` to close block");
        Block {
            items,
            range: self.finish(start),
        }
    }

    fn parse_block_item_at(&mut self, nesting: SyntaxNesting) -> BlockItem {
        match self.kind() {
            TokenKind::LetKw | TokenKind::VarKw | TokenKind::ScopedKw => {
                BlockItem::Local(self.parse_local_at(nesting))
            }
            TokenKind::ForKw => BlockItem::ForRange(self.parse_for_range_at(nesting)),
            TokenKind::DeferKw => {
                self.bump();
                BlockItem::Defer(self.parse_block_at(nesting))
            }
            TokenKind::DiscardKw => {
                self.bump();
                BlockItem::Discard(self.parse_expr_at(true, nesting))
            }
            TokenKind::ReturnKw => BlockItem::Return(self.parse_return_at(nesting)),
            TokenKind::AssertKw => {
                self.bump();
                BlockItem::Assert(self.parse_expr_at(true, nesting))
            }
            _ => {
                let left = self.parse_expr_at(true, nesting);
                if self.eat(TokenKind::Eq).is_some() {
                    let start = left.range.start;
                    let value = self.parse_expr_at(true, nesting);
                    BlockItem::Assignment(Assignment {
                        target: left,
                        range: self.finish(start),
                        value,
                    })
                } else {
                    BlockItem::Expr(left)
                }
            }
        }
    }

    fn reject_bare_unit_tail(&mut self, body: &Block) {
        let Some(BlockItem::Expr(expression)) = body.items.last() else {
            return;
        };
        if is_bare_unit_expression(expression) {
            self.error_at(
                "UnexpectedToken",
                "a Unit-returning callable must omit the final bare Unit expression",
                expression.range,
            );
        }
    }

    fn parse_for_range_at(&mut self, nesting: SyntaxNesting) -> ForRange {
        let start = self.start();
        self.bump();
        let binding = self.parse_ident("loop binding after `for`");
        self.expect(TokenKind::InKw, "`in` after loop binding");
        let range_start = self.parse_expr_at(true, nesting);
        self.expect(TokenKind::DotDot, "`..` in half-open range");
        // As with `if` and `match` scrutinees, the following `{` starts the
        // loop body rather than a record literal on the range-end expression.
        let end = self.parse_expr_at(false, nesting);
        let body = self.parse_block_at(nesting);
        ForRange {
            binding,
            start: range_start,
            end,
            body,
            range: self.finish(start),
        }
    }

    fn parse_local_at(&mut self, nesting: SyntaxNesting) -> LocalBinding {
        let start = self.start();
        let mutable = self.kind() == TokenKind::VarKw;
        let scoped = self.kind() == TokenKind::ScopedKw;
        self.bump();
        let mut names = vec![self.parse_ident("local binding name")];
        while self.eat(TokenKind::Comma).is_some() {
            names.push(self.parse_ident("local binding name after `,`"));
        }
        if names.len() > 1 && (mutable || scoped) {
            self.error_at(
                "TupleBindingRequiresLet",
                "tuple destructuring currently uses immutable `let` bindings",
                self.finish(start),
            );
        }
        let annotation = if names.len() == 1 && scoped && !self.at(TokenKind::Eq) {
            Some(self.parse_type_at(nesting))
        } else if self.eat(TokenKind::Colon).is_some() {
            let annotation_start = self.last_start();
            let annotation = self.parse_type_at(nesting);
            self.error_at(
                "UnexpectedToken",
                "Core locals use inference and do not accept source type annotations",
                TextRange::new(annotation_start, annotation.range.end),
            );
            None
        } else {
            None
        };
        self.expect(TokenKind::Eq, "`=` in local binding");
        let value = self.parse_expr_at(true, nesting);
        LocalBinding {
            mutable,
            scoped,
            names,
            annotation,
            value,
            range: self.finish(start),
        }
    }

    fn parse_return_at(&mut self, nesting: SyntaxNesting) -> ReturnExpr {
        let start = self.start();
        self.bump();
        let value = if self.at_any(&[TokenKind::Separator, TokenKind::RBrace, TokenKind::Eof]) {
            None
        } else {
            Some(self.parse_expr_at(true, nesting))
        };
        ReturnExpr {
            value,
            range: self.finish(start),
        }
    }

    fn current_binary_op(&self) -> Option<(BinaryOp, u8)> {
        Some(match self.kind() {
            TokenKind::Star => (BinaryOp::Multiply, 70),
            TokenKind::Slash => (BinaryOp::Divide, 70),
            TokenKind::Plus => (BinaryOp::Add, 60),
            TokenKind::Minus => (BinaryOp::Subtract, 60),
            TokenKind::Lt => (BinaryOp::Less, 50),
            TokenKind::LtEq => (BinaryOp::LessEqual, 50),
            TokenKind::Gt => (BinaryOp::Greater, 50),
            TokenKind::GtEq => (BinaryOp::GreaterEqual, 50),
            TokenKind::EqEq => (BinaryOp::Equal, 40),
            TokenKind::NotEq => (BinaryOp::NotEqual, 40),
            TokenKind::AndAnd => (BinaryOp::And, 30),
            TokenKind::OrOr => (BinaryOp::Or, 20),
            _ => return None,
        })
    }

    fn parse_path(&mut self) -> Path {
        let start = self.start();
        let mut segments = vec![self.parse_ident("name")];
        while self.eat(TokenKind::Dot).is_some() {
            segments.push(self.parse_ident("name segment after `.`"));
        }
        Path {
            segments,
            range: self.finish(start),
        }
    }

    fn parse_ident(&mut self, role: &str) -> Ident {
        if self.at(TokenKind::Ident) {
            let token = self.bump();
            Ident {
                text: token.text,
                range: token.range,
            }
        } else {
            let range = self.current_range();
            self.error_here("UnexpectedToken", format!("expected {role}"));
            Ident {
                text: String::new(),
                range: TextRange::new(range.start, range.start),
            }
        }
    }

    fn finish_braced_item(&mut self, description: &str) {
        if self.eat(TokenKind::Comma).is_some() || self.at(TokenKind::Separator) {
            self.skip_separators();
        } else if !self.at_any(&[TokenKind::RBrace, TokenKind::Eof]) {
            self.error_here(
                "UnexpectedToken",
                format!("expected a comma or newline after {description}"),
            );
            self.recover_to_list_boundary();
            if self.eat(TokenKind::Comma).is_some() || self.at(TokenKind::Separator) {
                self.skip_separators();
            }
        }
    }

    fn require_boundary(&mut self, description: &str) {
        if self.at(TokenKind::Separator) {
            self.skip_separators();
        } else if !self.at(TokenKind::Eof) {
            self.error_here(
                "UnexpectedToken",
                format!("expected a newline after {description}"),
            );
            self.recover_to_top_level();
        }
    }

    fn enter_nesting(
        &mut self,
        nesting: SyntaxNesting,
        construct: &'static str,
    ) -> Option<SyntaxNesting> {
        if let Some(nested) = nesting.nested() {
            return Some(nested);
        }
        self.error_here(
            "SyntaxNestingLimit",
            format!(
                "{construct} exceeds syntax nesting limit {MAX_SYNTAX_NESTING} (contract version {SYNTAX_NESTING_LIMIT_VERSION})"
            ),
        );
        self.recover_from_nesting_limit();
        None
    }

    /// Skips one over-deep error island without recursion. Matched delimiters
    /// that begin inside the island are consumed, while the first unmatched
    /// closer remains available to the bounded caller. A physical line before
    /// a declaration is always a synchronization point, even when the lexer
    /// classified it as continuation because a delimiter was left open.
    fn recover_from_nesting_limit(&mut self) {
        let mut index = self.current_index();
        let mut delimiter_depth = 0_usize;
        let mut consumed = false;
        let mut saw_physical_line_break = false;
        let mut last_end = self.last_end;

        while let Some(token) = self.tokens.get(index) {
            let kind = token.kind;
            if kind == TokenKind::Eof {
                self.pos = index;
                self.last_end = last_end;
                return;
            }

            if matches!(kind, TokenKind::Newline | TokenKind::Separator) {
                if kind == TokenKind::Separator && delimiter_depth == 0 && consumed {
                    self.pos = index;
                    self.last_end = last_end;
                    return;
                }
                saw_physical_line_break = true;
                index += 1;
                continue;
            }
            if kind.is_trivia() {
                index += 1;
                continue;
            }
            if saw_physical_line_break && self.is_top_start_at(index) {
                self.pos = index;
                self.last_end = last_end;
                return;
            }
            saw_physical_line_break = false;

            if delimiter_depth == 0
                && consumed
                && matches!(
                    kind,
                    TokenKind::Comma
                        | TokenKind::RParen
                        | TokenKind::RBracket
                        | TokenKind::RBrace
                        | TokenKind::FatArrow
                )
            {
                self.pos = index;
                self.last_end = last_end;
                return;
            }

            match kind {
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                    delimiter_depth += 1;
                }
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                    if delimiter_depth == 0 {
                        self.pos = index;
                        self.last_end = last_end;
                        return;
                    }
                    delimiter_depth -= 1;
                }
                _ => {}
            }
            consumed = true;
            last_end = token.range.end;
            index += 1;
        }
    }

    fn recover_to_top_level(&mut self) {
        let mut index = self.pos.min(self.tokens.len().saturating_sub(1));
        let mut saw_boundary = false;
        while index < self.tokens.len() {
            let kind = self.tokens[index].kind;
            if kind == TokenKind::Eof {
                self.pos = index;
                return;
            }
            if matches!(kind, TokenKind::Newline | TokenKind::Separator) {
                saw_boundary = true;
                index += 1;
                continue;
            }
            if kind.is_trivia() {
                index += 1;
                continue;
            }
            if saw_boundary && (self.is_top_start_at(index) || kind == TokenKind::ImportKw) {
                self.pos = index;
                return;
            }
            saw_boundary = false;
            index += 1;
        }
    }

    fn recover_to_list_boundary(&mut self) {
        while !self.at_any(&[
            TokenKind::Separator,
            TokenKind::Comma,
            TokenKind::RBrace,
            TokenKind::Eof,
        ]) {
            self.bump();
        }
    }

    fn recover_to_block_boundary(&mut self) {
        while !self.at_any(&[TokenKind::Separator, TokenKind::RBrace, TokenKind::Eof]) {
            self.bump();
        }
    }

    fn recover_to_impl_member(&mut self) {
        self.recover_to_member(Self::is_inherent_member_start);
    }

    fn recover_to_conformance_member(&mut self) {
        self.recover_to_member(Self::is_conformance_member_start);
    }

    fn recover_to_concept_member(&mut self) {
        self.recover_to_member(Self::is_concept_member_start);
    }

    fn recover_to_member(&mut self, is_member: impl Fn(&Self) -> bool) {
        let mut index = self.pos.min(self.tokens.len().saturating_sub(1));
        let mut saw_boundary = false;
        while index < self.tokens.len() {
            let kind = self.tokens[index].kind;
            if matches!(kind, TokenKind::Eof | TokenKind::RBrace) {
                self.pos = index;
                return;
            }
            if matches!(kind, TokenKind::Newline | TokenKind::Separator) {
                saw_boundary = true;
                index += 1;
                continue;
            }
            if kind.is_trivia() {
                index += 1;
                continue;
            }
            if saw_boundary {
                let saved = self.pos;
                self.pos = index;
                let found = is_member(self) || self.is_top_start();
                self.pos = saved;
                if found {
                    self.pos = index;
                    return;
                }
            }
            saw_boundary = false;
            index += 1;
        }
    }

    fn is_top_start(&self) -> bool {
        match self.kind() {
            TokenKind::TypeKw
            | TokenKind::RecordKw
            | TokenKind::EnumKw
            | TokenKind::FnKw
            | TokenKind::AsyncKw
            | TokenKind::ImplKw
            | TokenKind::ConceptKw => true,
            TokenKind::DynKw => self.nth_kind(1) == TokenKind::ConceptKw,
            TokenKind::TestKw => {
                self.nth_kind(1) == TokenKind::FnKw
                    || (self.nth_kind(1) == TokenKind::AsyncKw
                        && self.nth_kind(2) == TokenKind::FnKw)
            }
            TokenKind::PubKw => match self.nth_kind(1) {
                TokenKind::TypeKw
                | TokenKind::RecordKw
                | TokenKind::EnumKw
                | TokenKind::FnKw
                | TokenKind::AsyncKw
                | TokenKind::ConceptKw => true,
                TokenKind::DynKw => self.nth_kind(2) == TokenKind::ConceptKw,
                _ => false,
            },
            _ => false,
        }
    }

    fn is_top_start_at(&self, index: usize) -> bool {
        let kind = self.tokens[index].kind;
        match kind {
            TokenKind::TypeKw
            | TokenKind::RecordKw
            | TokenKind::EnumKw
            | TokenKind::FnKw
            | TokenKind::AsyncKw
            | TokenKind::ImplKw
            | TokenKind::ConceptKw => true,
            TokenKind::DynKw => self.raw_same_line_nth_kind(index, 1) == Some(TokenKind::ConceptKw),
            TokenKind::TestKw => {
                self.raw_same_line_nth_kind(index, 1) == Some(TokenKind::FnKw)
                    || (self.raw_same_line_nth_kind(index, 1) == Some(TokenKind::AsyncKw)
                        && self.raw_same_line_nth_kind(index, 2) == Some(TokenKind::FnKw))
            }
            TokenKind::PubKw => match self.raw_same_line_nth_kind(index, 1) {
                Some(
                    TokenKind::TypeKw
                    | TokenKind::RecordKw
                    | TokenKind::EnumKw
                    | TokenKind::FnKw
                    | TokenKind::AsyncKw
                    | TokenKind::ConceptKw,
                ) => true,
                Some(TokenKind::DynKw) => {
                    self.raw_same_line_nth_kind(index, 2) == Some(TokenKind::ConceptKw)
                }
                _ => false,
            },
            _ => false,
        }
    }

    fn raw_same_line_nth_kind(&self, start: usize, n: usize) -> Option<TokenKind> {
        let mut index = start;
        let mut remaining = n;
        while remaining > 0 {
            index += 1;
            while let Some(token) = self.tokens.get(index) {
                match token.kind {
                    TokenKind::Whitespace => index += 1,
                    TokenKind::Newline | TokenKind::Separator | TokenKind::Eof => return None,
                    _ => break,
                }
            }
            remaining -= 1;
        }
        self.tokens.get(index).map(|token| token.kind)
    }

    fn is_inherent_member_start(&self) -> bool {
        self.at(TokenKind::MethodKw)
            || (self.at(TokenKind::PubKw) && self.nth_kind(1) == TokenKind::MethodKw)
    }

    fn is_conformance_member_start(&self) -> bool {
        self.at(TokenKind::AssociatedKw)
            || self.at(TokenKind::MethodKw)
            || (self.at(TokenKind::StaticKw) && self.nth_kind(1) == TokenKind::MethodKw)
    }

    fn is_concept_member_start(&self) -> bool {
        self.is_conformance_member_start()
    }

    fn is_declaration_sync_start(&self) -> bool {
        self.is_top_start() || self.is_inherent_member_start() || self.is_conformance_member_start()
    }

    fn starts_type(&self) -> bool {
        matches!(
            self.kind(),
            TokenKind::Ident | TokenKind::LParen | TokenKind::Lt | TokenKind::DynKw
        )
    }

    fn at_expr_boundary(&self) -> bool {
        self.at_any(&[
            TokenKind::Separator,
            TokenKind::Comma,
            TokenKind::RParen,
            TokenKind::RBracket,
            TokenKind::RBrace,
            TokenKind::FatArrow,
            TokenKind::Eof,
        ])
    }

    fn skip_separators(&mut self) {
        while self.eat(TokenKind::Separator).is_some() {}
    }

    fn ensure_progress(&mut self, before: usize) {
        if self.pos == before && !self.at(TokenKind::Eof) {
            self.bump();
        }
    }

    fn error_expr(&self) -> Expr {
        let range = self.current_range();
        Expr {
            kind: ExprKind::Error,
            range: TextRange::new(range.start, range.start),
        }
    }

    fn error_expr_from(&self, start: u32) -> Expr {
        Expr {
            kind: ExprKind::Error,
            range: self.finish(start),
        }
    }

    fn error_type_from(&self, start: u32) -> TypeExpr {
        TypeExpr {
            kind: TypeExprKind::Error,
            range: self.finish(start),
        }
    }

    fn error_here(&mut self, code: &'static str, message: impl Into<String>) {
        let code = if code == "UnexpectedToken" && self.at(TokenKind::Eof) {
            "UnexpectedEndOfFile"
        } else {
            code
        };
        self.error_at(code, message, self.current_range());
    }

    fn error_at(&mut self, code: &'static str, message: impl Into<String>, range: TextRange) {
        self.diagnostics.push(Diagnostic::error(
            code,
            message,
            Span {
                file: self.file,
                range,
            },
        ));
    }

    fn expect(&mut self, kind: TokenKind, description: &str) -> Option<Token> {
        if self.at(kind) {
            Some(self.bump())
        } else {
            self.error_here(
                "UnexpectedToken",
                format!("expected {description}, found {}", self.describe_current()),
            );
            None
        }
    }

    fn describe_current(&self) -> String {
        if self.at(TokenKind::Eof) {
            "end of file".to_owned()
        } else {
            format!("`{}`", self.current_token().text)
        }
    }

    fn eat(&mut self, kind: TokenKind) -> Option<Token> {
        if self.at(kind) {
            Some(self.bump())
        } else {
            None
        }
    }

    fn bump(&mut self) -> Token {
        let index = self.current_index();
        let token = self.tokens[index].clone();
        self.pos = (index + 1).min(self.tokens.len());
        self.last_end = token.range.end;
        token
    }

    fn kind(&self) -> TokenKind {
        self.current_token().kind
    }

    fn nth_kind(&self, n: usize) -> TokenKind {
        let mut index = self.current_index();
        let mut remaining = n;
        while remaining > 0 {
            index = (index + 1).min(self.tokens.len().saturating_sub(1));
            while self.tokens[index].kind.is_trivia() {
                index = (index + 1).min(self.tokens.len().saturating_sub(1));
            }
            remaining -= 1;
        }
        self.tokens[index].kind
    }

    fn at(&self, kind: TokenKind) -> bool {
        self.kind() == kind
    }

    fn at_any(&self, kinds: &[TokenKind]) -> bool {
        kinds.contains(&self.kind())
    }

    fn current_token(&self) -> &Token {
        &self.tokens[self.current_index()]
    }

    fn current_index(&self) -> usize {
        let mut index = self.pos.min(self.tokens.len().saturating_sub(1));
        while self.tokens[index].kind.is_trivia() {
            index = (index + 1).min(self.tokens.len().saturating_sub(1));
        }
        index
    }

    fn has_physical_line_break_before_current(&self) -> bool {
        self.tokens[self.pos.min(self.tokens.len())..self.current_index()]
            .iter()
            .any(|token| matches!(token.kind, TokenKind::Newline | TokenKind::Separator))
    }

    fn current_range(&self) -> TextRange {
        self.current_token().range
    }

    fn start(&self) -> u32 {
        self.current_range().start
    }

    fn last_start(&self) -> u32 {
        self.tokens
            .get(self.pos.saturating_sub(1))
            .map_or(self.last_end, |token| token.range.start)
    }

    fn finish(&self, start: u32) -> TextRange {
        TextRange::new(start, self.last_end.max(start))
    }
}

fn is_bare_unit_type(ty: &TypeExpr) -> bool {
    matches!(
        &ty.kind,
        TypeExprKind::Named { path, arguments }
            if arguments.is_empty() && is_unqualified_unit_path(path)
    )
}

fn is_bare_unit_expression(expression: &Expr) -> bool {
    matches!(&expression.kind, ExprKind::Name(path) if is_unqualified_unit_path(path))
}

fn is_unqualified_unit_path(path: &Path) -> bool {
    matches!(path.segments.as_slice(), [segment] if segment.text == "Unit")
}

fn expr_as_path(expr: &Expr) -> Option<Path> {
    match &expr.kind {
        ExprKind::Name(path) => Some(path.clone()),
        ExprKind::Member { receiver, name } => {
            let mut path = expr_as_path(receiver)?;
            path.segments.push(name.clone());
            path.range.end = name.range.end;
            Some(path)
        }
        _ => None,
    }
}

fn to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}
