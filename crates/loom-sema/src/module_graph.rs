//! Deterministic module dependency graph construction.

use std::collections::{BTreeMap, BTreeSet};

use loom_core::{Diagnostic, ModuleName};
use loom_hir::{Import, ModuleId, ModuleResolution, Path, Program};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ImportEdge {
    pub from: ModuleId,
    pub to: ModuleId,
    pub import_index: usize,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ModuleGraph {
    outgoing: BTreeMap<ModuleId, Vec<ImportEdge>>,
}

impl ModuleGraph {
    #[must_use]
    pub fn outgoing(&self, module: ModuleId) -> &[ImportEdge] {
        self.outgoing.get(&module).map_or(&[], Vec::as_slice)
    }

    #[must_use]
    pub fn imports(&self, from: ModuleId, to: ModuleId) -> bool {
        self.outgoing(from).iter().any(|edge| edge.to == to)
    }

    /// Produces a dependency-first order when the graph is acyclic.
    #[must_use]
    pub fn dependency_order(&self, program: &Program) -> Option<Vec<ModuleId>> {
        let mut state = BTreeMap::new();
        let mut order = Vec::new();
        let mut modules = program.modules.iter().map(|(id, _)| id).collect::<Vec<_>>();
        modules.sort_by(|left, right| {
            program.modules[*left]
                .name
                .cmp(&program.modules[*right].name)
        });

        for module in modules {
            if !visit_for_order(module, self, &mut state, &mut order) {
                return None;
            }
        }
        Some(order)
    }
}

#[derive(Clone, Debug, Default)]
pub struct ModuleGraphBuild {
    pub graph: ModuleGraph,
    pub diagnostics: Vec<Diagnostic>,
}

impl ModuleGraphBuild {
    #[must_use]
    pub fn build(program: &Program) -> Self {
        let mut build = Self::default();
        let mut modules = program.modules.iter().map(|(id, _)| id).collect::<Vec<_>>();
        modules.sort_by(|left, right| {
            program.modules[*left]
                .name
                .cmp(&program.modules[*right].name)
        });

        for module in modules {
            let mut seen_targets = BTreeSet::new();
            for (import_index, import) in program.modules[module].imports.iter().enumerate() {
                if crate::std_primitives::resolve_import(program, module, &import.path).is_some()
                    || is_compiler_known_import(&import.path)
                {
                    continue;
                }
                let Some(imported_name) = imported_module_name(&import.path) else {
                    build.diagnostics.push(Diagnostic::error(
                        "UnexpectedToken",
                        "an import must contain a module and a declaration name",
                        import.span,
                    ));
                    continue;
                };
                let target = match program.resolve_module_from(module, &imported_name) {
                    ModuleResolution::Found(target) => target,
                    ModuleResolution::UndeclaredDependency(package) => {
                        build.diagnostics.push(Diagnostic::error(
                            "UndeclaredDependency",
                            format!(
                                "module `{imported_name}` belongs to `{package}`, which is not a direct dependency"
                            ),
                            import.span,
                        ));
                        continue;
                    }
                    ModuleResolution::Missing => {
                        build.diagnostics.push(Diagnostic::error(
                            "UnknownName",
                            format!("module `{imported_name}` does not exist"),
                            import.span,
                        ));
                        continue;
                    }
                };
                if seen_targets.insert((target, import_index)) {
                    build
                        .graph
                        .outgoing
                        .entry(module)
                        .or_default()
                        .push(ImportEdge {
                            from: module,
                            to: target,
                            import_index,
                        });
                }
            }
        }

        build.sort_edges(program);
        build.report_cycles(program);
        build
    }

    fn sort_edges(&mut self, program: &Program) {
        for edges in self.graph.outgoing.values_mut() {
            edges.sort_by(|left, right| {
                program.modules[left.to]
                    .name
                    .cmp(&program.modules[right.to].name)
                    .then(left.import_index.cmp(&right.import_index))
            });
        }
    }

    fn report_cycles(&mut self, program: &Program) {
        let mut state = BTreeMap::new();
        let mut stack = Vec::new();
        let mut reported = BTreeSet::new();
        let mut modules = program.modules.iter().map(|(id, _)| id).collect::<Vec<_>>();
        modules.sort_by(|left, right| {
            program.modules[*left]
                .name
                .cmp(&program.modules[*right].name)
        });

        for module in modules {
            self.visit_for_cycles(module, program, &mut state, &mut stack, &mut reported);
        }
    }

    fn visit_for_cycles(
        &mut self,
        module: ModuleId,
        program: &Program,
        state: &mut BTreeMap<ModuleId, VisitState>,
        stack: &mut Vec<ModuleId>,
        reported: &mut BTreeSet<Vec<ModuleId>>,
    ) {
        match state.get(&module) {
            Some(VisitState::Done | VisitState::Visiting) => return,
            None => {}
        }
        state.insert(module, VisitState::Visiting);
        stack.push(module);

        let outgoing = self.graph.outgoing(module).to_vec();
        for edge in outgoing {
            if state.get(&edge.to) == Some(&VisitState::Visiting) {
                let start = stack
                    .iter()
                    .position(|candidate| *candidate == edge.to)
                    .unwrap_or(0);
                let mut cycle = stack[start..].to_vec();
                canonicalize_cycle(&mut cycle, program);
                if reported.insert(cycle.clone()) {
                    let import = &program.modules[module].imports[edge.import_index];
                    let display = cycle
                        .iter()
                        .map(|id| program.modules[*id].name.as_str())
                        .chain(std::iter::once(program.modules[cycle[0]].name.as_str()))
                        .collect::<Vec<_>>()
                        .join(" -> ");
                    self.diagnostics.push(Diagnostic::error(
                        "ModuleCycle",
                        format!("module import cycle: {display}"),
                        import.span,
                    ));
                }
            } else if state.get(&edge.to) != Some(&VisitState::Done) {
                self.visit_for_cycles(edge.to, program, state, stack, reported);
            }
        }

        stack.pop();
        state.insert(module, VisitState::Done);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VisitState {
    Visiting,
    Done,
}

fn visit_for_order(
    module: ModuleId,
    graph: &ModuleGraph,
    state: &mut BTreeMap<ModuleId, VisitState>,
    order: &mut Vec<ModuleId>,
) -> bool {
    match state.get(&module) {
        Some(VisitState::Done) => return true,
        Some(VisitState::Visiting) => return false,
        None => {}
    }
    state.insert(module, VisitState::Visiting);
    for edge in graph.outgoing(module) {
        if !visit_for_order(edge.to, graph, state, order) {
            return false;
        }
    }
    state.insert(module, VisitState::Done);
    order.push(module);
    true
}

fn imported_module_name(path: &Path) -> Option<ModuleName> {
    if path.segments.len() < 2 {
        return None;
    }
    Some(ModuleName::new(
        path.segments[..path.segments.len() - 1]
            .iter()
            .map(|segment| segment.name.as_str())
            .collect::<Vec<_>>()
            .join("."),
    ))
}

pub(crate) fn imported_name(import: &Import) -> Option<&loom_core::Name> {
    import.path.last()
}

pub(crate) fn is_compiler_known_import(path: &Path) -> bool {
    matches!(
        path.as_string().as_str(),
        "std.time.milliseconds"
            | "std.file.open_read"
            | "std.file.create"
            | "std.file.open_read_path"
            | "std.file.create_path"
            | "std.file.try_open_read"
            | "std.file.try_create"
            | "std.file.try_open_read_path"
            | "std.file.try_create_path"
            | "std.net.connect"
            | "std.net.try_connect"
            | "std.json.format_json"
            | "std.log.write"
    )
}

fn canonicalize_cycle(cycle: &mut [ModuleId], program: &Program) {
    let Some((start, _)) = cycle
        .iter()
        .enumerate()
        .min_by_key(|(_, module)| &program.modules[**module].name)
    else {
        return;
    };
    cycle.rotate_left(start);
}

#[cfg(test)]
mod tests {
    use loom_core::{FileId, LOOM_LANGUAGE_VERSION, ModuleName, Name, PackageId, Span};
    use loom_hir::{Import, Path, PathSegment, Program};

    use super::ModuleGraphBuild;

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

    #[test]
    fn reports_import_cycle_once_in_canonical_order() {
        let mut program = Program::default();
        let a = program.intern_module(ModuleName::new("a"), FileId(1), Span::new(FileId(1), 0, 8));
        let b = program.intern_module(ModuleName::new("b"), FileId(2), Span::new(FileId(2), 0, 8));
        program.modules[a]
            .imports
            .push(import(FileId(1), "b", "Thing"));
        program.modules[b]
            .imports
            .push(import(FileId(2), "a", "Thing"));

        let build = ModuleGraphBuild::build(&program);
        assert_eq!(build.diagnostics.len(), 1);
        assert_eq!(build.diagnostics[0].code, "ModuleCycle");
        assert!(build.graph.dependency_order(&program).is_none());
    }

    #[test]
    fn process_primitives_are_skipped_only_for_the_exact_std_owner() {
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

        let build = ModuleGraphBuild::build(&program);
        assert!(build.graph.outgoing(process).is_empty());
        assert!(build.graph.imports(wrong_owner, process));
        assert!(build.graph.imports(wrong_package, wrong_package));
        assert_eq!(build.graph.outgoing(application).len(), 2);
        assert!(build.graph.imports(application, process));
        assert!(
            build
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "ModuleCycle")
        );
    }
}
