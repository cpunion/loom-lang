use loom_core::{FileId, ModuleName, Span};
use loom_hir::{DefId, DefinitionTag, Expr, ModuleId, Path, Pattern, TypeRef};
use loom_sema::{CallTarget, Namespace, PlaceProjection, Resolution, TyData};
use loom_syntax::TokenKind;

use crate::AnalysisSnapshot;

/// Returns whether `text` is one non-keyword Unicode XID identifier.
#[must_use]
pub fn is_valid_identifier(text: &str) -> bool {
    let lexed = loom_syntax::lex(text);
    lexed.errors.is_empty()
        && matches!(lexed.tokens.as_slice(), [token, eof] if token.kind == TokenKind::Ident && eof.kind == TokenKind::Eof)
}

/// A globally named declaration resolved by semantic identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SymbolInfo {
    pub id: DefId,
    pub name: String,
    pub module: String,
    pub kind: &'static str,
    /// Exact declaration-name span, rather than the declaration body.
    pub definition: Span,
}

/// One semantically resolved source occurrence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SymbolReference {
    pub span: Span,
    pub is_declaration: bool,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Occurrence {
    target: DefId,
    span: Span,
    declaration: bool,
}

impl AnalysisSnapshot {
    /// Resolves a global declaration at a source position. Locals and
    /// parameters deliberately remain outside this first rename index.
    #[must_use]
    pub fn definition_at(&self, file: FileId, byte: u32) -> Option<SymbolInfo> {
        let mut targets = self
            .symbol_occurrences()
            .into_iter()
            .filter(|occurrence| {
                occurrence.span.file == file
                    && occurrence.span.range.start <= byte
                    && byte < occurrence.span.range.end
            })
            .map(|occurrence| occurrence.target)
            .collect::<Vec<_>>();
        targets.sort_unstable();
        targets.dedup();
        let [target] = targets.as_slice() else {
            return None;
        };
        self.symbol_info(*target)
    }

    /// Returns every indexed global occurrence for the declaration at the
    /// requested position. The result is stable in path/span order.
    #[must_use]
    pub fn references_at(
        &self,
        file: FileId,
        byte: u32,
        include_declaration: bool,
    ) -> Option<Vec<SymbolReference>> {
        let symbol = self.definition_at(file, byte)?;
        Some(
            self.symbol_occurrences()
                .into_iter()
                .filter(|occurrence| {
                    occurrence.target == symbol.id
                        && (include_declaration || !occurrence.declaration)
                })
                .map(|occurrence| SymbolReference {
                    span: occurrence.span,
                    is_declaration: occurrence.declaration,
                })
                .collect(),
        )
    }

    fn symbol_info(&self, target: DefId) -> Option<SymbolInfo> {
        let definition = self.hir().definitions.get(target)?;
        let name = definition.name.as_ref()?;
        let declaration = self.hir().source_map.definition(target)?;
        let definition_span = self.ident_span(declaration, name.as_str(), false)?;
        Some(SymbolInfo {
            id: target,
            name: name.to_string(),
            module: self.hir().modules[definition.module].name.to_string(),
            kind: definition_kind_name(definition.kind.tag()),
            definition: definition_span,
        })
    }

    fn symbol_occurrences(&self) -> Vec<Occurrence> {
        let mut occurrences = Vec::new();
        self.collect_declarations(&mut occurrences);
        self.collect_imports(&mut occurrences);
        self.collect_type_references(&mut occurrences);
        self.collect_body_references(&mut occurrences);
        occurrences.sort_by_key(|occurrence| {
            let path = self
                .sources()
                .document(occurrence.span.file)
                .map_or("", crate::SourceDocument::relative_path);
            (
                path,
                occurrence.span.range.start,
                occurrence.span.range.end,
                occurrence.target.raw(),
                occurrence.declaration,
            )
        });
        occurrences.dedup();
        occurrences
    }

    fn collect_declarations(&self, occurrences: &mut Vec<Occurrence>) {
        for (target, definition) in self.hir().definitions.iter() {
            let (Some(name), Some(span)) = (
                definition.name.as_ref(),
                self.hir().source_map.definition(target),
            ) else {
                continue;
            };
            if let Some(span) = self.ident_span(span, name.as_str(), false) {
                occurrences.push(Occurrence {
                    target,
                    span,
                    declaration: true,
                });
            }
        }
    }

    fn collect_imports(&self, occurrences: &mut Vec<Occurrence>) {
        for (module, data) in self.hir().modules.iter() {
            for import in &data.imports {
                let Some(span) = import.path.segments.last().map(|segment| segment.span) else {
                    continue;
                };
                for target in self.resolve_across_namespaces(module, &import.path) {
                    occurrences.push(Occurrence {
                        target,
                        span,
                        declaration: false,
                    });
                }
            }
        }
    }

    fn collect_type_references(&self, occurrences: &mut Vec<Occurrence>) {
        for (reference, source) in self.hir().type_refs.iter() {
            let Some(ty) = self
                .semantic_analysis()
                .typed
                .resolved_type_refs
                .get(reference)
                .copied()
            else {
                continue;
            };
            let target = match self.semantic_analysis().typed.types.data(ty) {
                TyData::Nominal { definition, .. } => Some(*definition),
                TyData::DynTarget(instance) => Some(instance.concept),
                TyData::Projection {
                    associated_type, ..
                } => Some(*associated_type),
                _ => None,
            };
            let Some(target) = target else {
                continue;
            };
            let span = match source {
                TypeRef::Path(path) => path.segments.last().map(|segment| segment.span),
                TypeRef::Apply { constructor, .. } => {
                    constructor.segments.last().map(|segment| segment.span)
                }
                TypeRef::Dyn(concept) => concept.path.segments.last().map(|segment| segment.span),
                TypeRef::Projection { associated, .. } => self
                    .hir()
                    .source_map
                    .type_ref(reference)
                    .and_then(|span| self.ident_span(span, associated.as_str(), true)),
                TypeRef::Error
                | TypeRef::Tuple(_)
                | TypeRef::SelfType
                | TypeRef::View { .. }
                | TypeRef::UnavailableCarrier { .. } => None,
            };
            if let Some(span) = span {
                occurrences.push(Occurrence {
                    target,
                    span,
                    declaration: false,
                });
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn collect_body_references(&self, occurrences: &mut Vec<Occurrence>) {
        for (body_id, body) in self.hir().bodies.iter() {
            let Some(semantics) = self.semantic_analysis().typed.body(body_id) else {
                continue;
            };
            let module = self.hir().definitions[body.owner].module;
            for (expression, source) in body.expressions.iter() {
                match source {
                    Expr::Path(path) => {
                        if let Some(Resolution::Definition(target)) =
                            semantics.expression_resolutions.get(expression)
                        {
                            push_path(occurrences, *target, path);
                        }
                    }
                    Expr::Call { callee, .. } => {
                        let Some(target) = semantics
                            .calls
                            .get(expression)
                            .and_then(|call| call_target_definition(&call.target))
                        else {
                            continue;
                        };
                        if let Expr::Path(path) = &body.expressions[*callee] {
                            push_path(occurrences, target, path);
                        }
                    }
                    Expr::MethodCall { method, .. } => {
                        let Some(target) = semantics
                            .calls
                            .get(expression)
                            .and_then(|call| call_target_definition(&call.target))
                        else {
                            continue;
                        };
                        let Some(source_span) = body.source_map.expr(expression) else {
                            continue;
                        };
                        if let Some(span) = self.ident_span(source_span, method.as_str(), true) {
                            occurrences.push(Occurrence {
                                target,
                                span,
                                declaration: false,
                            });
                        }
                    }
                    Expr::Field { name, .. } => {
                        let Some(target) = semantics
                            .expression_places
                            .get(expression)
                            .and_then(|place| place.projections.last())
                            .map(|projection| match projection {
                                PlaceProjection::Field(target) => *target,
                            })
                        else {
                            continue;
                        };
                        let Some(source_span) = body.source_map.expr(expression) else {
                            continue;
                        };
                        if let Some(span) = self.ident_span(source_span, name.as_str(), true) {
                            occurrences.push(Occurrence {
                                target,
                                span,
                                declaration: false,
                            });
                        }
                    }
                    Expr::RecordLiteral { ty, fields } => {
                        if let Some(target) =
                            semantics.expression_types.get(expression).and_then(|ty| {
                                match self.semantic_analysis().typed.types.data(*ty) {
                                    TyData::Nominal { definition, .. } => Some(*definition),
                                    _ => None,
                                }
                            })
                        {
                            push_path(occurrences, target, ty);
                        }
                        if let Some(mapped) = semantics.record_fields.get(expression) {
                            for (target, value) in mapped {
                                let Some(field) = fields.iter().find(|field| field.value == *value)
                                else {
                                    continue;
                                };
                                if let Some(span) =
                                    self.ident_span(field.span, field.name.as_str(), false)
                                {
                                    occurrences.push(Occurrence {
                                        target: *target,
                                        span,
                                        declaration: false,
                                    });
                                }
                            }
                        }
                    }
                    Expr::View { concept, .. } => {
                        if let Some(target) =
                            self.resolve_unique(module, &concept.path, Namespace::Concept)
                        {
                            push_path(occurrences, target, &concept.path);
                        }
                    }
                    Expr::QualifiedMethodCall {
                        concept, method, ..
                    } => {
                        if let Some(target) = semantics
                            .calls
                            .get(expression)
                            .and_then(|call| call_target_definition(&call.target))
                            && let Some(source_span) = body.source_map.expr(expression)
                            && let Some(span) = self.ident_span(source_span, method.as_str(), true)
                        {
                            occurrences.push(Occurrence {
                                target,
                                span,
                                declaration: false,
                            });
                        }
                        if let Some(target) =
                            self.resolve_unique(module, &concept.path, Namespace::Concept)
                        {
                            push_path(occurrences, target, &concept.path);
                        }
                    }
                    _ => {}
                }
            }
            for (pattern, source) in body.patterns.iter() {
                let Some(Resolution::Definition(target)) =
                    semantics.pattern_resolutions.get(pattern)
                else {
                    continue;
                };
                if let Pattern::Name { path, .. } | Pattern::Variant { path, .. } = source {
                    push_path(occurrences, *target, path);
                }
            }
        }
    }

    fn resolve_across_namespaces(&self, module: ModuleId, path: &Path) -> Vec<DefId> {
        let mut targets = [Namespace::Type, Namespace::Value, Namespace::Concept]
            .into_iter()
            .filter_map(|namespace| self.resolve_unique(module, path, namespace))
            .collect::<Vec<_>>();
        targets.sort_unstable();
        targets.dedup();
        targets
    }

    fn resolve_unique(&self, module: ModuleId, path: &Path, namespace: Namespace) -> Option<DefId> {
        let name = &path.segments.last()?.name;
        let target_module = if path.segments.len() == 1 {
            module
        } else {
            let module_name = ModuleName::new(
                path.segments[..path.segments.len() - 1]
                    .iter()
                    .map(|segment| segment.name.as_str())
                    .collect::<Vec<_>>()
                    .join("."),
            );
            self.hir().module_by_name(&module_name)?
        };
        let map = self.semantic_analysis().def_maps.map(target_module)?;
        let binding = if path.segments.len() == 1 {
            map.resolve(namespace, name)
        } else {
            map.resolve_local(namespace, name)
        }?;
        binding.unique()
    }

    fn ident_span(&self, containing: Span, name: &str, prefer_last: bool) -> Option<Span> {
        let parse = self.parse(containing.file)?;
        let mut candidates = parse.tokens().iter().filter(|token| {
            token.kind == TokenKind::Ident
                && token.text == name
                && token.range.start >= containing.range.start
                && token.range.end <= containing.range.end
        });
        let range = if prefer_last {
            candidates.next_back()?.range
        } else {
            candidates.next()?.range
        };
        Some(Span {
            file: containing.file,
            range,
        })
    }
}

fn push_path(occurrences: &mut Vec<Occurrence>, target: DefId, path: &Path) {
    if let Some(segment) = path.segments.last() {
        occurrences.push(Occurrence {
            target,
            span: segment.span,
            declaration: false,
        });
    }
}

const fn call_target_definition(target: &CallTarget) -> Option<DefId> {
    match target {
        CallTarget::Function(definition)
        | CallTarget::InherentMethod(definition)
        | CallTarget::EnumVariant(definition)
        | CallTarget::RefinedConstructor(definition) => Some(*definition),
        CallTarget::StaticConcept { requirement } | CallTarget::DynamicConcept { requirement } => {
            Some(*requirement)
        }
        CallTarget::Builtin(_) | CallTarget::Error => None,
    }
}

const fn definition_kind_name(kind: DefinitionTag) -> &'static str {
    match kind {
        DefinitionTag::Error => "error",
        DefinitionTag::RefinedType => "constrained type",
        DefinitionTag::Record => "record",
        DefinitionTag::Field => "field",
        DefinitionTag::Enum => "enum",
        DefinitionTag::Variant => "enum variant",
        DefinitionTag::Function => "function",
        DefinitionTag::Test => "test",
        DefinitionTag::InherentImpl => "impl",
        DefinitionTag::Concept => "concept",
        DefinitionTag::AssociatedType => "associated type",
        DefinitionTag::Conformance => "conformance",
        DefinitionTag::Method => "method",
    }
}
