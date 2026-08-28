use std::collections::BTreeSet;

use loom_core::{FileId, ModuleName, Span};
use loom_hir::{
    BodyId, ConceptRef, DefId, DefinitionKind, DefinitionTag, Expr, GenericParamId, LocalId,
    ModuleId, ParamId, Path, Pattern, TypeRef, Visibility,
};
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

/// A named global or callable-local declaration resolved by semantic identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SymbolInfo {
    pub id: SymbolId,
    pub name: String,
    pub module: String,
    pub kind: &'static str,
    /// Exact declaration-name span, rather than the declaration body.
    pub definition: Span,
}

/// Stable semantic identity for global and callable-local source names.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SymbolId {
    Definition(DefId),
    GenericParam(GenericParamId),
    Param(ParamId),
    Local { body: BodyId, local: LocalId },
}

impl From<DefId> for SymbolId {
    fn from(value: DefId) -> Self {
        Self::Definition(value)
    }
}

/// One semantically resolved source occurrence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SymbolReference {
    pub span: Span,
    pub is_declaration: bool,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Occurrence {
    target: SymbolId,
    span: Span,
    declaration: bool,
}

impl AnalysisSnapshot {
    /// Resolves a global or callable-local declaration at a source position.
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

    /// Returns every indexed occurrence for the declaration at the requested
    /// position. The result is stable in path/span order.
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

    /// Returns every indexed declaration in deterministic source order.
    #[must_use]
    pub fn symbols(&self) -> Vec<SymbolInfo> {
        self.symbol_occurrences()
            .into_iter()
            .filter(|occurrence| occurrence.declaration)
            .filter_map(|occurrence| self.symbol_info(occurrence.target))
            .collect()
    }

    /// Returns indexed declarations whose exact name span belongs to `file`.
    #[must_use]
    pub fn document_symbols(&self, file: FileId) -> Vec<SymbolInfo> {
        self.symbols()
            .into_iter()
            .filter(|symbol| symbol.definition.file == file)
            .collect()
    }

    /// Returns declarations which are useful completion candidates at a
    /// source position. Callable-local names are limited to their body and to
    /// declarations preceding the cursor; global private names stay module-local.
    #[must_use]
    pub fn completion_symbols(&self, file: FileId, byte: u32) -> Vec<SymbolInfo> {
        let modules = self
            .hir()
            .modules
            .iter()
            .filter_map(|(module, data)| data.files.contains(&file).then_some(module))
            .collect::<BTreeSet<_>>();
        self.symbols()
            .into_iter()
            .filter(|symbol| match symbol.id {
                SymbolId::Definition(definition) => {
                    let definition = &self.hir().definitions[definition];
                    definition.visibility == Visibility::Public
                        || modules.contains(&definition.module)
                }
                SymbolId::Local { body, .. } => {
                    self.hir().source_map.body(body).is_some_and(|span| {
                        span.file == file
                            && span.range.start <= byte
                            && byte <= span.range.end
                            && symbol.definition.range.start <= byte
                    })
                }
                SymbolId::Param(parameter) => {
                    self.owner_body_contains(self.hir().params[parameter].owner, file, byte)
                }
                SymbolId::GenericParam(parameter) => {
                    self.owner_body_contains(self.hir().generic_params[parameter].owner, file, byte)
                }
            })
            .collect()
    }

    fn owner_body_contains(&self, owner: DefId, file: FileId, byte: u32) -> bool {
        self.hir().bodies.iter().any(|(body, data)| {
            data.owner == owner
                && self.hir().source_map.body(body).is_some_and(|span| {
                    span.file == file && span.range.start <= byte && byte <= span.range.end
                })
        })
    }

    fn symbol_info(&self, target: SymbolId) -> Option<SymbolInfo> {
        let (name, module, kind, declaration) = match target {
            SymbolId::Definition(target) => {
                let definition = self.hir().definitions.get(target)?;
                (
                    definition.name.as_ref()?.as_str(),
                    definition.module,
                    definition_kind_name(definition.kind.tag()),
                    self.hir().source_map.definition(target)?,
                )
            }
            SymbolId::GenericParam(target) => {
                let parameter = self.hir().generic_params.get(target)?;
                (
                    parameter.name.as_str(),
                    self.hir().definitions[parameter.owner].module,
                    "type parameter",
                    self.hir().source_map.generic_param(target)?,
                )
            }
            SymbolId::Param(target) => {
                let parameter = self.hir().params.get(target)?;
                (
                    parameter.name.as_str(),
                    self.hir().definitions[parameter.owner].module,
                    "parameter",
                    self.hir().source_map.param(target)?,
                )
            }
            SymbolId::Local {
                body,
                local: local_id,
            } => {
                let body = self.hir().bodies.get(body)?;
                let local = body.locals.get(local_id)?;
                (
                    local.name.as_str(),
                    self.hir().definitions[body.owner].module,
                    if local.mutable { "variable" } else { "local" },
                    body.source_map.local(local_id)?,
                )
            }
        };
        let definition_span = self.ident_span(declaration, name, false)?;
        Some(SymbolInfo {
            id: target,
            name: name.to_owned(),
            module: self.hir().modules[module].name.to_string(),
            kind,
            definition: definition_span,
        })
    }

    fn symbol_occurrences(&self) -> Vec<Occurrence> {
        let mut occurrences = Vec::new();
        self.collect_declarations(&mut occurrences);
        self.collect_imports(&mut occurrences);
        self.collect_type_references(&mut occurrences);
        self.collect_concept_references(&mut occurrences);
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
                occurrence.target,
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
                    target: target.into(),
                    span,
                    declaration: true,
                });
            }
        }
        for (target, parameter) in self.hir().generic_params.iter() {
            if let Some(span) = self
                .hir()
                .source_map
                .generic_param(target)
                .and_then(|span| self.ident_span(span, parameter.name.as_str(), false))
            {
                occurrences.push(Occurrence {
                    target: SymbolId::GenericParam(target),
                    span,
                    declaration: true,
                });
            }
        }
        for (target, parameter) in self.hir().params.iter() {
            if let Some(span) = self
                .hir()
                .source_map
                .param(target)
                .and_then(|span| self.ident_span(span, parameter.name.as_str(), false))
            {
                occurrences.push(Occurrence {
                    target: SymbolId::Param(target),
                    span,
                    declaration: true,
                });
            }
        }
        for (body_id, body) in self.hir().bodies.iter() {
            for (local_id, local) in body.locals.iter() {
                if let Some(span) = body
                    .source_map
                    .local(local_id)
                    .and_then(|span| self.ident_span(span, local.name.as_str(), false))
                {
                    occurrences.push(Occurrence {
                        target: SymbolId::Local {
                            body: body_id,
                            local: local_id,
                        },
                        span,
                        declaration: true,
                    });
                }
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
                        target: target.into(),
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
                TyData::Nominal { definition, .. } => Some((*definition).into()),
                TyData::DynTarget(instance) => Some(instance.concept.into()),
                TyData::Param(parameter) => Some(SymbolId::GenericParam(*parameter)),
                TyData::Projection {
                    associated_type, ..
                } => Some((*associated_type).into()),
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
                TypeRef::Error | TypeRef::Tuple(_) | TypeRef::SelfType => None,
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

    fn collect_concept_references(&self, occurrences: &mut Vec<Occurrence>) {
        for (_, definition) in self.hir().definitions.iter() {
            match &definition.kind {
                DefinitionKind::AssociatedType(associated) => {
                    for concept in &associated.bounds {
                        self.collect_concept_reference(occurrences, definition.module, concept);
                    }
                }
                DefinitionKind::Conformance(conformance) => self.collect_concept_reference(
                    occurrences,
                    definition.module,
                    &conformance.concept,
                ),
                _ => {}
            }
        }
        for (_, parameter) in self.hir().generic_params.iter() {
            let module = self.hir().definitions[parameter.owner].module;
            for concept in &parameter.bounds {
                self.collect_concept_reference(occurrences, module, concept);
            }
        }
    }

    fn collect_concept_reference(
        &self,
        occurrences: &mut Vec<Occurrence>,
        module: ModuleId,
        concept: &ConceptRef,
    ) {
        if let Some(target) = self.resolve_unique(module, &concept.path, Namespace::Concept) {
            push_path(occurrences, target, &concept.path);
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
                        if let Some(target) = semantics
                            .expression_resolutions
                            .get(expression)
                            .and_then(|resolution| resolution_symbol(*resolution, body_id))
                        {
                            push_symbol_path(occurrences, target, path);
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
                                target: target.into(),
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
                                target: target.into(),
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
                                        target: (*target).into(),
                                        span,
                                        declaration: false,
                                    });
                                }
                            }
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
                                target: target.into(),
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
            match self.hir().resolve_module_from(module, &module_name) {
                loom_hir::ModuleResolution::Found(module) => module,
                loom_hir::ModuleResolution::UndeclaredDependency(_)
                | loom_hir::ModuleResolution::Missing => return None,
            }
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
    push_symbol_path(occurrences, target.into(), path);
}

fn push_symbol_path(occurrences: &mut Vec<Occurrence>, target: SymbolId, path: &Path) {
    if let Some(segment) = path.segments.last() {
        occurrences.push(Occurrence {
            target,
            span: segment.span,
            declaration: false,
        });
    }
}

const fn resolution_symbol(resolution: Resolution, body: BodyId) -> Option<SymbolId> {
    match resolution {
        Resolution::Definition(definition) => Some(SymbolId::Definition(definition)),
        Resolution::GenericParam(parameter) => Some(SymbolId::GenericParam(parameter)),
        Resolution::Param(parameter) => Some(SymbolId::Param(parameter)),
        Resolution::Local(local) => Some(SymbolId::Local { body, local }),
        Resolution::SelfValue
        | Resolution::ResultValue
        | Resolution::Builtin(_)
        | Resolution::Error => None,
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
        CallTarget::Builtin(_) | CallTarget::StandardLibrary(_) | CallTarget::Error => None,
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
