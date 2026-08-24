//! Module namespaces and explicit import resolution.

use std::collections::BTreeMap;

use loom_core::{Diagnostic, Name, Span};
use loom_hir::{DefId, DefinitionKind, ModuleId, Program, Visibility};

use crate::module_graph::{imported_name, is_compiler_known_import};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Namespace {
    Type,
    Value,
    Concept,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Binding {
    Unique(DefId),
    Duplicate(Vec<DefId>),
}

impl Binding {
    #[must_use]
    pub fn unique(&self) -> Option<DefId> {
        match self {
            Self::Unique(definition) => Some(*definition),
            Self::Duplicate(_) => None,
        }
    }

    #[must_use]
    pub fn candidates(&self) -> &[DefId] {
        match self {
            Self::Unique(definition) => std::slice::from_ref(definition),
            Self::Duplicate(definitions) => definitions,
        }
    }

    fn merge(&mut self, definition: DefId) {
        match self {
            Self::Unique(previous) => {
                if *previous != definition {
                    *self = Self::Duplicate(vec![*previous, definition]);
                }
            }
            Self::Duplicate(definitions) => {
                if !definitions.contains(&definition) {
                    definitions.push(definition);
                    definitions.sort_unstable();
                }
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct DefMap {
    types: BTreeMap<Name, Binding>,
    values: BTreeMap<Name, Binding>,
    concepts: BTreeMap<Name, Binding>,
    local_types: BTreeMap<Name, Binding>,
    local_values: BTreeMap<Name, Binding>,
    local_concepts: BTreeMap<Name, Binding>,
}

impl DefMap {
    #[must_use]
    pub fn resolve(&self, namespace: Namespace, name: &Name) -> Option<&Binding> {
        self.namespace(namespace).get(name)
    }

    pub fn entries(&self, namespace: Namespace) -> impl Iterator<Item = (&Name, &Binding)> {
        self.namespace(namespace).iter()
    }

    #[must_use]
    pub fn resolve_local(&self, namespace: Namespace, name: &Name) -> Option<&Binding> {
        self.local_namespace(namespace).get(name)
    }

    fn namespace(&self, namespace: Namespace) -> &BTreeMap<Name, Binding> {
        match namespace {
            Namespace::Type => &self.types,
            Namespace::Value => &self.values,
            Namespace::Concept => &self.concepts,
        }
    }

    fn namespace_mut(&mut self, namespace: Namespace) -> &mut BTreeMap<Name, Binding> {
        match namespace {
            Namespace::Type => &mut self.types,
            Namespace::Value => &mut self.values,
            Namespace::Concept => &mut self.concepts,
        }
    }

    fn local_namespace(&self, namespace: Namespace) -> &BTreeMap<Name, Binding> {
        match namespace {
            Namespace::Type => &self.local_types,
            Namespace::Value => &self.local_values,
            Namespace::Concept => &self.local_concepts,
        }
    }

    fn local_namespace_mut(&mut self, namespace: Namespace) -> &mut BTreeMap<Name, Binding> {
        match namespace {
            Namespace::Type => &mut self.local_types,
            Namespace::Value => &mut self.local_values,
            Namespace::Concept => &mut self.local_concepts,
        }
    }

    fn insert(&mut self, namespace: Namespace, name: Name, definition: DefId) {
        self.namespace_mut(namespace)
            .entry(name)
            .and_modify(|binding| binding.merge(definition))
            .or_insert(Binding::Unique(definition));
    }

    fn insert_local(&mut self, namespace: Namespace, name: Name, definition: DefId) {
        self.local_namespace_mut(namespace)
            .entry(name.clone())
            .and_modify(|binding| binding.merge(definition))
            .or_insert(Binding::Unique(definition));
        self.insert(namespace, name, definition);
    }
}

#[derive(Clone, Debug, Default)]
pub struct DefMapBuild {
    pub maps: BTreeMap<ModuleId, DefMap>,
    pub diagnostics: Vec<Diagnostic>,
}

impl DefMapBuild {
    #[must_use]
    pub fn build(program: &Program) -> Self {
        let mut build = Self::default();
        build.collect_local_definitions(program);
        build.report_local_duplicates(program);
        build.resolve_imports(program);
        build
    }

    #[must_use]
    pub fn map(&self, module: ModuleId) -> Option<&DefMap> {
        self.maps.get(&module)
    }

    fn collect_local_definitions(&mut self, program: &Program) {
        let mut modules = program.modules.iter().map(|(id, _)| id).collect::<Vec<_>>();
        modules.sort_by(|left, right| {
            program.modules[*left]
                .name
                .cmp(&program.modules[*right].name)
        });
        for module in modules {
            let map = self.maps.entry(module).or_default();
            let mut definitions = program.modules[module].items.clone();
            definitions.sort_by_key(|definition| definition_sort_key(program, *definition));
            for definition in definitions {
                let Some(name) = program.definitions[definition].name.clone() else {
                    continue;
                };
                if let Some(namespace) = namespace_of(&program.definitions[definition].kind) {
                    map.insert_local(namespace, name, definition);
                }
            }
        }
    }

    fn report_local_duplicates(&mut self, program: &Program) {
        for (module, map) in &self.maps {
            for namespace in [Namespace::Type, Namespace::Value, Namespace::Concept] {
                for (name, binding) in map.entries(namespace) {
                    let Binding::Duplicate(candidates) = binding else {
                        continue;
                    };
                    let mut ordered = candidates.clone();
                    ordered.sort_by_key(|definition| definition_sort_key(program, *definition));
                    let primary = definition_span(program, ordered[0]);
                    let mut diagnostic = Diagnostic::error(
                        "DuplicateDeclaration",
                        format!(
                            "`{name}` is defined more than once in module `{}`",
                            program.modules[*module].name
                        ),
                        primary,
                    );
                    for definition in ordered.into_iter().skip(1) {
                        diagnostic = diagnostic.with_label(
                            definition_span(program, definition),
                            "conflicting definition",
                        );
                    }
                    self.diagnostics.push(diagnostic);
                }
            }
        }
    }

    fn resolve_imports(&mut self, program: &Program) {
        let local_maps = self.maps.clone();
        let mut modules = program.modules.iter().map(|(id, _)| id).collect::<Vec<_>>();
        modules.sort_by(|left, right| {
            program.modules[*left]
                .name
                .cmp(&program.modules[*right].name)
        });

        for module in modules {
            for import in &program.modules[module].imports {
                if is_compiler_known_import(&import.path) {
                    continue;
                }
                let Some(imported_name) = imported_name(import) else {
                    continue;
                };
                let Some(target_module) = import_target_module(program, module, import) else {
                    continue;
                };
                let Some(target_map) = local_maps.get(&target_module) else {
                    continue;
                };

                let mut found = false;
                for namespace in [Namespace::Type, Namespace::Value, Namespace::Concept] {
                    let Some(binding) = target_map.resolve_local(namespace, imported_name) else {
                        continue;
                    };
                    found = true;
                    for definition in binding.candidates() {
                        if program.definitions[*definition].visibility == Visibility::Public {
                            self.maps.entry(module).or_default().insert(
                                namespace,
                                imported_name.clone(),
                                *definition,
                            );
                        } else {
                            self.diagnostics.push(Diagnostic::error(
                                "NameNotVisible",
                                format!("`{imported_name}` is private"),
                                import.span,
                            ));
                        }
                    }
                }
                if !found {
                    self.diagnostics.push(Diagnostic::error(
                        "UnknownName",
                        format!(
                            "`{}` does not define `{imported_name}`",
                            program.modules[target_module].name
                        ),
                        import.span,
                    ));
                }
            }
        }
    }
}

fn namespace_of(kind: &DefinitionKind) -> Option<Namespace> {
    match kind {
        DefinitionKind::RefinedType(_) | DefinitionKind::Record(_) | DefinitionKind::Enum(_) => {
            Some(Namespace::Type)
        }
        DefinitionKind::Function(_) | DefinitionKind::Test(_) => Some(Namespace::Value),
        DefinitionKind::Concept(_) => Some(Namespace::Concept),
        DefinitionKind::Field(_)
        | DefinitionKind::Variant(_)
        | DefinitionKind::InherentImpl(_)
        | DefinitionKind::AssociatedType(_)
        | DefinitionKind::Conformance(_)
        | DefinitionKind::Method(_)
        | DefinitionKind::Error => None,
    }
}

fn import_target_module(
    program: &Program,
    from: ModuleId,
    import: &loom_hir::Import,
) -> Option<ModuleId> {
    if import.path.segments.len() < 2 {
        return None;
    }
    let module_name = loom_core::ModuleName::new(
        import.path.segments[..import.path.segments.len() - 1]
            .iter()
            .map(|segment| segment.name.as_str())
            .collect::<Vec<_>>()
            .join("."),
    );
    match program.resolve_module_from(from, &module_name) {
        loom_hir::ModuleResolution::Found(module) => Some(module),
        loom_hir::ModuleResolution::UndeclaredDependency(_)
        | loom_hir::ModuleResolution::Missing => None,
    }
}

fn definition_sort_key(program: &Program, definition: DefId) -> (u32, u32, u32) {
    let span = definition_span(program, definition);
    (span.file.0, span.range.start, definition.raw())
}

fn definition_span(program: &Program, definition: DefId) -> Span {
    program
        .source_map
        .definition(definition)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use loom_core::{FileId, ModuleName, Name, Span};
    use loom_hir::{
        CallableSignature, Definition, DefinitionKind, FunctionDef, Program, Visibility,
    };

    use super::{Binding, DefMapBuild, Namespace};

    #[test]
    fn duplicate_definitions_never_choose_a_winner() {
        let mut program = Program::default();
        let module = program.intern_module(
            ModuleName::new("example"),
            FileId(0),
            Span::new(FileId(0), 0, 14),
        );
        let body_owner = loom_hir::DefId::from_raw(0);
        let placeholder_body = loom_hir::Body {
            owner: body_owner,
            kind: loom_hir::BodyKind::Function,
            locals: loom_hir::Arena::default(),
            expressions: {
                let mut expressions = loom_hir::Arena::default();
                expressions.alloc(loom_hir::Expr::Literal(loom_hir::Literal::Unit));
                expressions
            },
            patterns: loom_hir::Arena::default(),
            root: loom_hir::ExprId::from_raw(0),
            source_map: loom_hir::BodySourceMap::default(),
        };
        let body = program.alloc_body(placeholder_body, Span::new(FileId(0), 20, 22));

        for start in [20, 40] {
            program.alloc_definition(
                Definition {
                    module,
                    name: Some(Name::new("same")),
                    visibility: Visibility::Private,
                    kind: DefinitionKind::Function(FunctionDef {
                        signature: CallableSignature::default(),
                        is_async: false,
                        body,
                    }),
                },
                Span::new(FileId(0), start, start + 4),
            );
        }

        let build = DefMapBuild::build(&program);
        assert_eq!(build.diagnostics.len(), 1);
        let binding = build
            .map(module)
            .unwrap()
            .resolve(Namespace::Value, &Name::new("same"))
            .unwrap();
        assert!(matches!(binding, Binding::Duplicate(candidates) if candidates.len() == 2));
    }
}
