//! Module namespaces and explicit import resolution.

use std::collections::BTreeMap;

use loom_core::{Diagnostic, FileId, Name, Span};
use loom_hir::{DefId, DefinitionKind, ModuleId, Program, Visibility};
use serde::{Deserialize, Serialize};

use crate::module_graph::{imported_name, is_compiler_known_import};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub enum Namespace {
    Type,
    Value,
    Concept,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
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

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct ScopeMap {
    types: BTreeMap<Name, Binding>,
    values: BTreeMap<Name, Binding>,
    concepts: BTreeMap<Name, Binding>,
}

impl ScopeMap {
    fn resolve(&self, namespace: Namespace, name: &Name) -> Option<&Binding> {
        self.namespace(namespace).get(name)
    }

    fn entries(&self, namespace: Namespace) -> impl Iterator<Item = (&Name, &Binding)> {
        self.namespace(namespace).iter()
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

    fn insert(&mut self, namespace: Namespace, name: Name, definition: DefId) {
        self.namespace_mut(namespace)
            .entry(name)
            .and_modify(|binding| binding.merge(definition))
            .or_insert(Binding::Unique(definition));
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct DefMap {
    /// Declarations owned by this nominal module. For a test companion these
    /// remain separate from the production fallback so companion helpers can
    /// shadow production names without creating duplicate bindings.
    local: ScopeMap,
    files: BTreeMap<FileId, ScopeMap>,
    production_fallback: Option<ScopeMap>,
}

impl DefMap {
    /// Resolves an unqualified name in one source file. Declarations from the
    /// directory package are shared, while imported bindings are file-local.
    #[must_use]
    pub fn resolve(&self, namespace: Namespace, name: &Name, file: FileId) -> Option<&Binding> {
        self.files
            .get(&file)
            .unwrap_or(&self.local)
            .resolve(namespace, name)
            .or_else(|| {
                self.production_fallback
                    .as_ref()
                    .and_then(|fallback| fallback.resolve(namespace, name))
            })
    }

    pub fn entries(
        &self,
        namespace: Namespace,
        file: FileId,
    ) -> impl Iterator<Item = (&Name, &Binding)> {
        let primary = self.files.get(&file).unwrap_or(&self.local);
        primary.entries(namespace).chain(
            self.production_fallback
                .iter()
                .flat_map(move |fallback| fallback.entries(namespace))
                .filter(move |(name, _)| primary.resolve(namespace, name).is_none()),
        )
    }

    #[must_use]
    pub fn resolve_local(&self, namespace: Namespace, name: &Name) -> Option<&Binding> {
        self.local.resolve(namespace, name).or_else(|| {
            self.production_fallback
                .as_ref()
                .and_then(|fallback| fallback.resolve(namespace, name))
        })
    }

    fn local_entries(&self, namespace: Namespace) -> impl Iterator<Item = (&Name, &Binding)> {
        self.local.entries(namespace)
    }

    fn insert_local(&mut self, namespace: Namespace, name: Name, definition: DefId) {
        self.local.insert(namespace, name, definition);
    }

    fn insert_import(&mut self, file: FileId, namespace: Namespace, name: Name, definition: DefId) {
        let local = self.local.clone();
        self.files
            .entry(file)
            .or_insert(local)
            .insert(namespace, name, definition);
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct DefMapBuild {
    pub maps: BTreeMap<ModuleId, DefMap>,
    pub diagnostics: Vec<Diagnostic>,
}

impl DefMapBuild {
    #[must_use]
    pub fn build(program: &Program) -> Self {
        let mut build = Self::default();
        build.collect_local_definitions(program);
        build.attach_test_companion_fallbacks(program);
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

    fn attach_test_companion_fallbacks(&mut self, program: &Program) {
        for (module, data) in program.modules.iter() {
            if !program.is_test_companion(module) {
                continue;
            }
            let production = program.production_module(module);
            let fallback = self
                .maps
                .get(&production)
                .map_or_else(ScopeMap::default, |map| map.local.clone());
            self.maps.entry(module).or_default().production_fallback = Some(fallback);
            debug_assert_eq!(
                data.package, program.modules[production].package,
                "a test companion belongs to its production package"
            );
        }
    }

    fn report_local_duplicates(&mut self, program: &Program) {
        for (module, map) in &self.maps {
            for namespace in [Namespace::Type, Namespace::Value, Namespace::Concept] {
                for (name, binding) in map.local_entries(namespace) {
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
                if crate::std_primitives::resolve_import(program, module, &import.path).is_some()
                    || is_compiler_known_import(&import.path)
                {
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
                        if program.definitions[*definition].visibility == Visibility::Public
                            || program
                                .can_access_private(module, program.definitions[*definition].module)
                        {
                            self.maps.entry(module).or_default().insert_import(
                                import.file,
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
        DefinitionKind::Constant(_) | DefinitionKind::Function(_) | DefinitionKind::Test(_) => {
            Some(Namespace::Value)
        }
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
    use loom_core::{FileId, LOOM_LANGUAGE_VERSION, ModuleName, Name, PackageId, Span};
    use loom_hir::{
        CallableSignature, Definition, DefinitionKind, FunctionDef, Import, Path, PathSegment,
        Program, Visibility,
    };

    use super::{Binding, DefMapBuild, Namespace};

    fn import(file: FileId, module: &str, item: &str) -> Import {
        let span = Span::new(file, 0, 10);
        Import {
            file,
            path: Path {
                segments: module
                    .split('.')
                    .chain(std::iter::once(item))
                    .map(|name| PathSegment {
                        name: Name::new(name),
                        span,
                    })
                    .collect(),
            },
            span,
        }
    }

    fn public_function(
        program: &mut Program,
        module: loom_hir::ModuleId,
        name: &str,
    ) -> loom_hir::DefId {
        program.alloc_definition(
            Definition {
                module,
                name: Some(Name::new(name)),
                visibility: Visibility::Public,
                kind: DefinitionKind::Function(FunctionDef {
                    signature: CallableSignature::default(),
                    is_async: false,
                    body: loom_hir::BodyId::from_raw(0),
                }),
            },
            Span::new(FileId(0), 0, 1),
        )
    }

    fn private_function(
        program: &mut Program,
        module: loom_hir::ModuleId,
        file: FileId,
        name: &str,
    ) -> loom_hir::DefId {
        program.alloc_definition(
            Definition {
                module,
                name: Some(Name::new(name)),
                visibility: Visibility::Private,
                kind: DefinitionKind::Function(FunctionDef {
                    signature: CallableSignature::default(),
                    is_async: false,
                    body: loom_hir::BodyId::from_raw(0),
                }),
            },
            Span::new(file, 0, 1),
        )
    }

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
            .resolve(Namespace::Value, &Name::new("same"), FileId(0))
            .unwrap();
        assert!(matches!(binding, Binding::Duplicate(candidates) if candidates.len() == 2));
    }

    #[test]
    fn imports_are_file_local_while_directory_declarations_are_shared() {
        let mut program = Program::default();
        let application_package = PackageId::new("application", "0");
        let dependency_package = PackageId::new("dependency", "0");
        let first_file = FileId(1);
        let second_file = FileId(2);
        let application = program.intern_package_module(
            application_package.clone(),
            ModuleName::new("application"),
            first_file,
            Span::new(first_file, 0, 1),
        );
        program.intern_package_module(
            application_package.clone(),
            ModuleName::new("application"),
            second_file,
            Span::new(second_file, 0, 1),
        );
        let dependency = program.intern_package_module(
            dependency_package.clone(),
            ModuleName::new("dependency.tools"),
            FileId(3),
            Span::new(FileId(3), 0, 1),
        );
        let imported = public_function(&mut program, dependency, "answer");
        let shared = private_function(&mut program, application, second_file, "shared");
        program.modules[application]
            .imports
            .push(import(first_file, "dep.tools", "answer"));
        program.register_package(dependency_package.clone(), [], false);
        program.register_package(
            application_package,
            [(Name::new("dep"), dependency_package)],
            true,
        );

        let build = DefMapBuild::build(&program);
        assert!(build.diagnostics.is_empty(), "{:#?}", build.diagnostics);
        let map = build.map(application).expect("application definition map");
        assert_eq!(
            map.resolve(Namespace::Value, &Name::new("answer"), first_file)
                .and_then(Binding::unique),
            Some(imported)
        );
        assert!(
            map.resolve(Namespace::Value, &Name::new("answer"), second_file)
                .is_none(),
            "an import from the first file must not leak into the second file"
        );
        for file in [first_file, second_file] {
            assert_eq!(
                map.resolve(Namespace::Value, &Name::new("shared"), file)
                    .and_then(Binding::unique),
                Some(shared),
                "directory-package declarations remain shared"
            );
        }
    }

    #[test]
    fn process_primitive_import_authority_does_not_create_public_bindings() {
        let mut program = Program::default();
        let std_package = PackageId::compiler_std(LOOM_LANGUAGE_VERSION);
        let hostile_package = PackageId::new("hostile-std", "0");
        let application_package = PackageId::new("application", "0");
        let process = program.intern_package_module(
            std_package.clone(),
            ModuleName::new("std.process"),
            FileId(1),
            Span::new(FileId(1), 0, 10),
        );
        let wrong_owner = program.intern_package_module(
            std_package.clone(),
            ModuleName::new("std.other"),
            FileId(2),
            Span::new(FileId(2), 0, 10),
        );
        let wrong_package = program.intern_package_module(
            hostile_package.clone(),
            ModuleName::new("std.process"),
            FileId(3),
            Span::new(FileId(3), 0, 10),
        );
        let application = program.intern_package_module(
            application_package.clone(),
            ModuleName::new("application"),
            FileId(4),
            Span::new(FileId(4), 0, 10),
        );
        let arguments = public_function(&mut program, process, "arguments");
        program.modules[process]
            .imports
            .push(import(FileId(1), "std.process", "__argument_count"));
        program.modules[process]
            .imports
            .push(import(FileId(1), "std.process", "__argument_at"));
        program.modules[process]
            .imports
            .push(import(FileId(1), "std.process", "__environment"));
        program.modules[wrong_owner].imports.push(import(
            FileId(2),
            "std.process",
            "__environment",
        ));
        program.modules[wrong_package].imports.push(import(
            FileId(3),
            "std.process",
            "__argument_count",
        ));
        program.modules[application].imports.push(import(
            FileId(4),
            "std.process",
            "__argument_count",
        ));
        program.modules[application]
            .imports
            .push(import(FileId(4), "std.process", "arguments"));
        program.register_package(std_package.clone(), [], false);
        program.register_package(hostile_package, [], false);
        program.register_package(application_package, [(Name::new("std"), std_package)], true);

        let build = DefMapBuild::build(&program);
        assert_eq!(
            build
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == "UnknownName")
                .count(),
            3,
            "{:#?}",
            build.diagnostics
        );
        assert!(
            build
                .map(process)
                .unwrap()
                .resolve(Namespace::Value, &Name::new("__argument_count"), FileId(1),)
                .is_none()
        );
        assert_eq!(
            build
                .map(application)
                .unwrap()
                .resolve(Namespace::Value, &Name::new("arguments"), FileId(4))
                .and_then(Binding::unique),
            Some(arguments)
        );
    }
}
