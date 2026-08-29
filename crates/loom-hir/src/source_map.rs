//! Source provenance for lowered HIR.

use loom_core::Span;

use crate::{ArenaMap, BodyId, DefId, ExprId, LocalId, ModuleId, ParamId, PatternId, TypeRefId};

/// Program-wide source provenance.
#[derive(Clone, Debug, Default)]
pub struct ProgramSourceMap {
    module_files: ArenaMap<ModuleId, Vec<Span>>,
    definitions: ArenaMap<DefId, Span>,
    generic_params: ArenaMap<crate::GenericParamId, Span>,
    params: ArenaMap<ParamId, Span>,
    type_refs: ArenaMap<TypeRefId, Span>,
    bodies: ArenaMap<BodyId, Span>,
}

impl ProgramSourceMap {
    pub fn add_module_file(&mut self, module: ModuleId, span: Span) {
        if let Some(spans) = self.module_files.get_mut(module) {
            spans.push(span);
        } else {
            self.module_files.insert(module, vec![span]);
        }
    }

    #[must_use]
    pub fn module_files(&self, module: ModuleId) -> &[Span] {
        self.module_files.get(module).map_or(&[], Vec::as_slice)
    }

    pub fn insert_definition(&mut self, definition: DefId, span: Span) {
        self.definitions.insert(definition, span);
    }

    #[must_use]
    pub fn definition(&self, definition: DefId) -> Option<Span> {
        self.definitions.get(definition).copied()
    }

    pub fn insert_generic_param(&mut self, parameter: crate::GenericParamId, span: Span) {
        self.generic_params.insert(parameter, span);
    }

    #[must_use]
    pub fn generic_param(&self, parameter: crate::GenericParamId) -> Option<Span> {
        self.generic_params.get(parameter).copied()
    }

    pub fn insert_param(&mut self, parameter: ParamId, span: Span) {
        self.params.insert(parameter, span);
    }

    #[must_use]
    pub fn param(&self, parameter: ParamId) -> Option<Span> {
        self.params.get(parameter).copied()
    }

    pub fn insert_type_ref(&mut self, ty: TypeRefId, span: Span) {
        self.type_refs.insert(ty, span);
    }

    #[must_use]
    pub fn type_ref(&self, ty: TypeRefId) -> Option<Span> {
        self.type_refs.get(ty).copied()
    }

    pub fn insert_body(&mut self, body: BodyId, span: Span) {
        self.bodies.insert(body, span);
    }

    #[must_use]
    pub fn body(&self, body: BodyId) -> Option<Span> {
        self.bodies.get(body).copied()
    }
}

/// Body-local source provenance.
#[derive(Clone, Debug, Default)]
pub struct BodySourceMap {
    expressions: ArenaMap<ExprId, Span>,
    patterns: ArenaMap<PatternId, Span>,
    locals: ArenaMap<LocalId, Span>,
}

impl BodySourceMap {
    pub fn insert_expr(&mut self, expression: ExprId, span: Span) {
        self.expressions.insert(expression, span);
    }

    #[must_use]
    pub fn expr(&self, expression: ExprId) -> Option<Span> {
        self.expressions.get(expression).copied()
    }

    pub fn insert_pattern(&mut self, pattern: PatternId, span: Span) {
        self.patterns.insert(pattern, span);
    }

    #[must_use]
    pub fn pattern(&self, pattern: PatternId) -> Option<Span> {
        self.patterns.get(pattern).copied()
    }

    pub fn insert_local(&mut self, local: LocalId, span: Span) {
        self.locals.insert(local, span);
    }

    #[must_use]
    pub fn local(&self, local: LocalId) -> Option<Span> {
        self.locals.get(local).copied()
    }
}
