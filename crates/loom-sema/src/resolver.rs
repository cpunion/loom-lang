//! Contextual name resolution over deterministic module maps.

use std::collections::BTreeMap;

use loom_core::{ModuleName, Name};
use loom_hir::{DefId, GenericParamId, LocalId, ModuleId, ParamId, Path, Program, Visibility};

use crate::{Binding, DefMapBuild, Namespace, Resolution};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolveError {
    Missing,
    Duplicate(Vec<DefId>),
    Private(DefId),
    UnknownModule(ModuleName),
}

pub struct Resolver<'a> {
    program: &'a Program,
    def_maps: &'a DefMapBuild,
    module: ModuleId,
    generic_params: BTreeMap<Name, GenericParamId>,
    params: BTreeMap<Name, ParamId>,
    scopes: Vec<BTreeMap<Name, LocalId>>,
    allow_self: bool,
    allow_result: bool,
}

impl<'a> Resolver<'a> {
    #[must_use]
    pub fn new(program: &'a Program, def_maps: &'a DefMapBuild, module: ModuleId) -> Self {
        Self {
            program,
            def_maps,
            module,
            generic_params: BTreeMap::new(),
            params: BTreeMap::new(),
            scopes: vec![BTreeMap::new()],
            allow_self: false,
            allow_result: false,
        }
    }

    pub fn add_generic_param(&mut self, name: Name, parameter: GenericParamId) {
        self.generic_params.insert(name, parameter);
    }

    pub fn add_param(&mut self, name: Name, parameter: ParamId) {
        self.params.insert(name, parameter);
    }

    pub fn set_allow_self(&mut self, allow: bool) {
        self.allow_self = allow;
    }

    pub fn set_allow_result(&mut self, allow: bool) {
        self.allow_result = allow;
    }

    pub fn push_scope(&mut self) {
        self.scopes.push(BTreeMap::new());
    }

    /// Pops the current lexical scope.
    ///
    /// # Panics
    ///
    /// Panics if called for the resolver's permanent root scope.
    pub fn pop_scope(&mut self) {
        assert!(self.scopes.len() > 1, "cannot pop the root resolver scope");
        self.scopes.pop();
    }

    /// Adds a local to the current scope.
    ///
    /// # Panics
    ///
    /// Panics only if the resolver's internal root-scope invariant is broken.
    pub fn add_local(&mut self, name: Name, local: LocalId) -> Option<LocalId> {
        self.scopes
            .last_mut()
            .expect("resolver always has a root scope")
            .insert(name, local)
    }

    /// Resolves a value path in lexical and module scope.
    ///
    /// # Errors
    ///
    /// Returns the precise lookup failure when the name is missing,
    /// duplicated, private, or names an unknown module.
    pub fn resolve_value(&self, path: &Path) -> Result<Resolution, ResolveError> {
        if path.segments.len() == 1 {
            let name = &path.segments[0].name;
            if name.as_str() == "self" && self.allow_self {
                return Ok(Resolution::SelfValue);
            }
            if name.as_str() == "result" && self.allow_result {
                return Ok(Resolution::ResultValue);
            }
            for scope in self.scopes.iter().rev() {
                if let Some(local) = scope.get(name) {
                    return Ok(Resolution::Local(*local));
                }
            }
            if let Some(parameter) = self.params.get(name) {
                return Ok(Resolution::Param(*parameter));
            }
        }
        self.resolve_definition(path, Namespace::Value)
            .map(Resolution::Definition)
    }

    /// Resolves a type path or in-scope generic parameter.
    ///
    /// # Errors
    ///
    /// Returns the precise lookup failure when the path cannot name one
    /// visible, unique type.
    pub fn resolve_type(&self, path: &Path) -> Result<Resolution, ResolveError> {
        if path.segments.len() == 1
            && let Some(parameter) = self.generic_params.get(&path.segments[0].name)
        {
            return Ok(Resolution::GenericParam(*parameter));
        }
        self.resolve_definition(path, Namespace::Type)
            .map(Resolution::Definition)
    }

    /// Resolves a concept path.
    ///
    /// # Errors
    ///
    /// Returns the precise lookup failure when the path cannot name one
    /// visible, unique concept.
    pub fn resolve_concept(&self, path: &Path) -> Result<DefId, ResolveError> {
        self.resolve_definition(path, Namespace::Concept)
    }

    /// Resolves a path in one module namespace.
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError`] when the path is missing, duplicated, private,
    /// or contains an unknown module prefix.
    pub fn resolve_definition(
        &self,
        path: &Path,
        namespace: Namespace,
    ) -> Result<DefId, ResolveError> {
        let Some(name) = path.last() else {
            return Err(ResolveError::Missing);
        };
        if path.segments.len() == 1 {
            return binding_result(
                self.def_maps
                    .map(self.module)
                    .and_then(|map| map.resolve(namespace, name)),
            );
        }

        let module_name = ModuleName::new(
            path.segments[..path.segments.len() - 1]
                .iter()
                .map(|segment| segment.name.as_str())
                .collect::<Vec<_>>()
                .join("."),
        );
        let Some(module) = self.program.module_by_name(&module_name) else {
            return Err(ResolveError::UnknownModule(module_name));
        };
        let definition = binding_result(
            self.def_maps
                .map(module)
                .and_then(|map| map.resolve_local(namespace, name)),
        )?;
        if module != self.module
            && self.program.definitions[definition].visibility != Visibility::Public
        {
            return Err(ResolveError::Private(definition));
        }
        Ok(definition)
    }
}

fn binding_result(binding: Option<&Binding>) -> Result<DefId, ResolveError> {
    match binding {
        Some(Binding::Unique(definition)) => Ok(*definition),
        Some(Binding::Duplicate(definitions)) => Err(ResolveError::Duplicate(definitions.clone())),
        None => Err(ResolveError::Missing),
    }
}

#[cfg(test)]
mod tests {
    use loom_core::{FileId, ModuleName, Name, Span};
    use loom_hir::{
        Arena, Body, BodyKind, BodySourceMap, CallableSignature, Definition, DefinitionKind, Expr,
        FunctionDef, Literal, Program, Visibility,
    };

    use super::{ResolveError, Resolver};
    use crate::{DefMapBuild, Resolution};

    #[test]
    fn local_scope_shadows_parameter_without_changing_module_map() {
        let mut program = Program::default();
        let module =
            program.intern_module(ModuleName::new("m"), FileId(0), Span::new(FileId(0), 0, 8));
        let mut expressions = Arena::default();
        let root = expressions.alloc(Expr::Literal(Literal::Unit));
        let body = program.alloc_body(
            Body {
                owner: loom_hir::DefId::from_raw(0),
                kind: BodyKind::Function,
                locals: Arena::default(),
                expressions,
                patterns: Arena::default(),
                root,
                source_map: BodySourceMap::default(),
            },
            Span::new(FileId(0), 20, 22),
        );
        program.alloc_definition(
            Definition {
                module,
                name: Some(Name::new("f")),
                visibility: Visibility::Private,
                kind: DefinitionKind::Function(FunctionDef {
                    signature: CallableSignature::default(),
                    is_async: false,
                    body,
                }),
            },
            Span::new(FileId(0), 10, 22),
        );
        let maps = DefMapBuild::build(&program);
        let mut resolver = Resolver::new(&program, &maps, module);
        let parameter = loom_hir::ParamId::from_raw(0);
        let local = loom_hir::LocalId::from_raw(0);
        resolver.add_param(Name::new("value"), parameter);
        resolver.add_local(Name::new("value"), local);
        let path = loom_hir::Path::from_name(Name::new("value"), Span::new(FileId(0), 30, 35));

        assert_eq!(resolver.resolve_value(&path), Ok(Resolution::Local(local)));
        assert_eq!(resolver.resolve_type(&path), Err(ResolveError::Missing));
    }
}
