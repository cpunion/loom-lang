//! Whole-program semantic analysis entry point.

use std::collections::{BTreeMap, BTreeSet};

use loom_core::{Diagnostic, Name, Severity, Span};
use loom_hir::{
    BinaryOp, BodyId, BodyKind, DefId, DefinitionKind, Expr, ExprId, GenericParamId, Literal,
    LocalId, MatchArm, ModuleId, ParamId, Path, Pattern, PatternId, Program, ReceiverKind,
    Statement, TaskJoinMode, TypeArgumentRef, TypeRef, TypeRefId, UnaryOp,
};

use crate::{
    AssociatedTypeBinding, BodySemantics, Bound, BuiltinType, BuiltinValue, CallResolution,
    CallTarget, CallableSignature, Coercion, ConceptInstance, DefMapBuild, Goal, ImplHeader,
    ModuleGraph, ModuleGraphBuild, Mutability, Namespace, ParamEnv, Place, PlaceProjection,
    PlaceRoot, ReceiverPassing, RegionId, Resolution, ResolveError, ScopedDisposal, Signature,
    SolveFailure, Substitution, TyData, TyId, TypedProgram, ViewResolution, ViewSource,
    ViewTokenId, WitnessSelection, WitnessSource,
};

const RESOURCE_MODULE: &str = "standard.resource";
const DISPOSE_CONCEPT: &str = "Dispose";
const MUST_SCOPE_CONCEPT: &str = "MustScope";
const NO_SUSPEND_CONCEPT: &str = "NoSuspend";

/// Complete checker output. A typed program remains available after errors for
/// diagnostics and editor features, but only an error-free analysis is
/// executable.
#[derive(Clone, Debug)]
pub struct Analysis {
    pub typed: TypedProgram,
    pub module_graph: ModuleGraph,
    pub def_maps: DefMapBuild,
    pub impl_index: crate::ImplIndex,
    pub diagnostics: Vec<Diagnostic>,
}

impl Analysis {
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
    }
}

/// Resolves modules, declarations and all declared types, then checks bodies.
#[must_use]
pub fn analyze(program: &Program) -> Analysis {
    let graph = ModuleGraphBuild::build(program);
    let def_maps = DefMapBuild::build(program);
    let mut diagnostics = graph.diagnostics;
    diagnostics.extend(def_maps.diagnostics.iter().cloned());

    let (typed, impl_index, mut diagnostics) = {
        let mut analyzer = Analyzer {
            program,
            def_maps: &def_maps,
            typed: TypedProgram::default(),
            impl_index: crate::ImplIndex::default(),
            diagnostics,
        };
        analyzer.collect_signatures();
        analyzer.validate_dynamic_concepts();
        analyzer.build_conformances();
        analyzer.validate_resource_concepts();
        analyzer.validate_async_functions();
        analyzer.check_bodies();
        (analyzer.typed, analyzer.impl_index, analyzer.diagnostics)
    };
    sort_diagnostics(&mut diagnostics);

    Analysis {
        typed,
        module_graph: graph.graph,
        def_maps,
        impl_index,
        diagnostics,
    }
}

struct Analyzer<'a> {
    program: &'a Program,
    def_maps: &'a DefMapBuild,
    typed: TypedProgram,
    impl_index: crate::ImplIndex,
    diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Debug)]
struct TypeContext {
    module: ModuleId,
    generic_params: BTreeMap<Name, GenericParamId>,
    self_ty: Option<TyId>,
    self_concept: Option<DefId>,
}

impl Analyzer<'_> {
    fn language_concept(&self, name: &str) -> Option<DefId> {
        self.program
            .definitions
            .iter()
            .find_map(|(definition, item)| {
                let module = &self.program.modules[item.module];
                (module.name.as_str() == RESOURCE_MODULE
                    && item
                        .name
                        .as_ref()
                        .is_some_and(|candidate| candidate.as_str() == name)
                    && matches!(item.kind, DefinitionKind::Concept(_)))
                .then_some(definition)
            })
    }

    fn validate_resource_concepts(&mut self) {
        for marker in [MUST_SCOPE_CONCEPT, NO_SUSPEND_CONCEPT] {
            let Some(definition) = self.language_concept(marker) else {
                continue;
            };
            let DefinitionKind::Concept(concept) = &self.program.definitions[definition].kind
            else {
                continue;
            };
            if concept.dyn_capable
                || !concept.associated_types.is_empty()
                || !concept.requirements.is_empty()
            {
                self.error(
                    "InvalidResourceMarker",
                    format!("standard.resource.{marker} must be an empty, non-dyn marker concept"),
                    self.definition_span(definition),
                );
            }
        }

        let Some(definition) = self.language_concept(DISPOSE_CONCEPT) else {
            return;
        };
        let DefinitionKind::Concept(concept) = &self.program.definitions[definition].kind else {
            return;
        };
        let valid_header = !concept.dyn_capable
            && concept.associated_types.is_empty()
            && concept.requirements.len() == 1;
        let valid_method = concept.requirements.first().is_some_and(|requirement| {
            let name_ok = self.program.definitions[*requirement]
                .name
                .as_ref()
                .is_some_and(|name| name.as_str() == "dispose");
            let source_ok = match &self.program.definitions[*requirement].kind {
                DefinitionKind::Method(method) => {
                    method.signature.receiver == Some(ReceiverKind::Mutable)
                        && method.signature.generic_params.is_empty()
                        && method.signature.params.is_empty()
                        && method.signature.contracts.requires.is_empty()
                        && method.signature.contracts.ensures.is_empty()
                }
                _ => false,
            };
            let signature_ok = matches!(
                self.typed.signatures.get(*requirement),
                Some(Signature::Callable(signature))
                    if !signature.is_async
                        && signature.return_ty == self.typed.types.builtin(BuiltinType::Unit)
            );
            name_ok && source_ok && signature_ok
        });
        if !valid_header || !valid_method {
            self.error(
                "InvalidDisposeConcept",
                "standard.resource.Dispose must be a non-dyn concept containing only `method dispose(mut self) Unit` without contracts",
                self.definition_span(definition),
            );
        }
    }

    fn validate_async_functions(&mut self) {
        let async_functions = self
            .program
            .definitions
            .iter()
            .filter_map(|(definition, item)| match &item.kind {
                DefinitionKind::Function(function) | DefinitionKind::Test(function)
                    if function.is_async =>
                {
                    Some(definition)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for definition in async_functions {
            let Some(Signature::Callable(signature)) = self.typed.signatures.get(definition) else {
                continue;
            };
            let (DefinitionKind::Function(source) | DefinitionKind::Test(source)) =
                &self.program.definitions[definition].kind
            else {
                continue;
            };
            if !signature.generic_params.is_empty()
                || !signature.bounds.is_empty()
                || signature.receiver.is_some()
                || !source.signature.contracts.requires.is_empty()
                || !source.signature.contracts.ensures.is_empty()
            {
                self.error(
                    "AsyncExecutableSliceRestriction",
                    "the current async executable slice accepts non-generic functions without receiver or contracts",
                    self.definition_span(definition),
                );
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn collect_signatures(&mut self) {
        // Shape declarations first so source order never affects type lookup.
        for (definition, item) in self.program.definitions.iter() {
            let signature = match &item.kind {
                DefinitionKind::RefinedType(_) => Some(Signature::Type {
                    generic_params: Vec::new(),
                }),
                DefinitionKind::Record(record) => Some(Signature::Type {
                    generic_params: record.generic_params.clone(),
                }),
                DefinitionKind::Enum(enumeration) => Some(Signature::Type {
                    generic_params: enumeration.generic_params.clone(),
                }),
                DefinitionKind::Concept(_) => Some(Signature::Concept),
                DefinitionKind::InherentImpl(_) | DefinitionKind::Conformance(_) => {
                    Some(Signature::Impl)
                }
                DefinitionKind::Error
                | DefinitionKind::Field(_)
                | DefinitionKind::Variant(_)
                | DefinitionKind::Function(_)
                | DefinitionKind::Test(_)
                | DefinitionKind::AssociatedType(_)
                | DefinitionKind::Method(_) => None,
            };
            if let Some(signature) = signature {
                self.typed.signatures.insert(definition, signature);
            }
        }

        for (definition, item) in self.program.definitions.iter() {
            let context = self.type_context(definition);
            match &item.kind {
                DefinitionKind::RefinedType(refined) => {
                    self.resolve_type_ref(refined.base, &context);
                }
                DefinitionKind::Record(record) => {
                    self.validate_generic_params(&record.generic_params);
                }
                DefinitionKind::Field(field) => {
                    let ty = self.resolve_type_ref(field.ty, &context);
                    self.typed.signatures.insert(
                        definition,
                        Signature::Field {
                            owner: field.owner,
                            ty,
                        },
                    );
                }
                DefinitionKind::Enum(enumeration) => {
                    self.validate_generic_params(&enumeration.generic_params);
                }
                DefinitionKind::Variant(variant) => {
                    let payload = variant
                        .payload
                        .iter()
                        .map(|ty| self.resolve_type_ref(*ty, &context))
                        .collect();
                    self.typed.signatures.insert(
                        definition,
                        Signature::Variant {
                            owner: variant.owner,
                            payload,
                        },
                    );
                }
                DefinitionKind::Function(function) | DefinitionKind::Test(function) => {
                    let mut signature =
                        self.resolve_callable(definition, &function.signature, &context);
                    signature.is_async = function.is_async;
                    if matches!(item.kind, DefinitionKind::Test(_)) {
                        self.validate_test_signature(definition, &signature);
                    }
                    self.typed
                        .signatures
                        .insert(definition, Signature::Callable(signature));
                }
                DefinitionKind::InherentImpl(implementation) => {
                    self.validate_generic_params(&implementation.generic_params);
                    let target = self.resolve_type_ref(implementation.target, &context);
                    let owned = match self.typed.types.data(target) {
                        TyData::Nominal { definition, .. } => {
                            self.program.definitions[*definition].module == item.module
                        }
                        _ => false,
                    };
                    if !owned {
                        self.error(
                            "ForeignInherentImpl",
                            "inherent impl must be declared in its target type's module",
                            self.definition_span(definition),
                        );
                    }
                }
                DefinitionKind::Concept(concept) => {
                    self.validate_unique_members(
                        definition,
                        concept.associated_types.iter().chain(&concept.requirements),
                    );
                }
                DefinitionKind::AssociatedType(associated) => {
                    let associated_self = if matches!(
                        self.program.definitions[associated.owner].kind,
                        DefinitionKind::Concept(_)
                    ) {
                        let self_ty = context.self_ty.unwrap_or_else(|| self.typed.types.error());
                        self.typed.types.intern(TyData::Projection {
                            self_ty,
                            concept: associated.owner,
                            associated_type: definition,
                        })
                    } else {
                        context.self_ty.unwrap_or_else(|| self.typed.types.error())
                    };
                    let bounds = associated
                        .bounds
                        .iter()
                        .filter_map(|bound| {
                            self.resolve_concept_ref(bound, &context, false)
                                .map(|concept| Bound {
                                    self_ty: associated_self,
                                    concept,
                                })
                        })
                        .collect();
                    if let Some(binding) = associated.binding {
                        self.resolve_type_ref(binding, &context);
                    }
                    self.typed.signatures.insert(
                        definition,
                        Signature::AssociatedType {
                            owner: associated.owner,
                            bounds,
                        },
                    );
                }
                DefinitionKind::Conformance(conformance) => {
                    self.validate_generic_params(&conformance.generic_params);
                    self.resolve_type_ref(conformance.target, &context);
                    self.resolve_concept_ref(&conformance.concept, &context, false);
                    self.validate_unique_members(
                        definition,
                        conformance
                            .associated_types
                            .iter()
                            .chain(&conformance.methods),
                    );
                }
                DefinitionKind::Method(method) => {
                    let signature = self.resolve_callable(definition, &method.signature, &context);
                    self.typed
                        .signatures
                        .insert(definition, Signature::Callable(signature));
                }
                DefinitionKind::Error => {}
            }
        }
    }

    fn resolve_callable(
        &mut self,
        owner: DefId,
        source: &loom_hir::CallableSignature,
        context: &TypeContext,
    ) -> CallableSignature {
        self.validate_generic_params(&source.generic_params);
        let params = source
            .params
            .iter()
            .map(|parameter| {
                let source = &self.program.params[*parameter];
                let ty = self.resolve_parameter_type(source.ty, context);
                (*parameter, ty)
            })
            .collect();
        let return_ty = if let Some(ty) = source.return_ty {
            self.resolve_type_ref(ty, context)
        } else {
            self.typed.types.builtin(BuiltinType::Unit)
        };
        let generic_params = self.generic_ids_for(owner);
        let mut bounds = Vec::new();
        let mut call_bounds = Vec::new();
        let call_generic_params = source.generic_params.clone();
        for parameter in &generic_params {
            let parameter_ty = self.typed.types.intern(TyData::Param(*parameter));
            for bound in &self.program.generic_params[*parameter].bounds {
                if let Some(concept) = self.resolve_concept_ref(bound, context, false) {
                    let resolved = Bound {
                        self_ty: parameter_ty,
                        concept,
                    };
                    if call_generic_params.contains(parameter) {
                        call_bounds.push(resolved.clone());
                    }
                    bounds.push(resolved);
                }
            }
        }
        CallableSignature {
            is_async: false,
            generic_params,
            call_generic_params,
            receiver: source.receiver,
            params,
            return_ty,
            bounds,
            call_bounds,
        }
    }

    /// Resolves an ordinary parameter type. A `dyn concept` name in this
    /// position is an ergonomic concept parameter, so `value Display` and
    /// `value dyn Display` share the same semantic representation. Nominal
    /// types continue through the regular type namespace unchanged.
    fn resolve_parameter_type(&mut self, reference: TypeRefId, context: &TypeContext) -> TyId {
        let source = self.program.type_refs[reference].clone();
        let candidate = match source {
            TypeRef::Path(path) => Some(loom_hir::ConceptRef {
                path,
                bindings: Vec::new(),
            }),
            TypeRef::Apply {
                constructor,
                arguments,
            } => {
                let bindings = arguments
                    .iter()
                    .filter_map(|argument| match argument {
                        TypeArgumentRef::Binding(binding) => Some(binding.clone()),
                        TypeArgumentRef::Type(_) => None,
                    })
                    .collect::<Vec<_>>();
                (!bindings.is_empty()).then_some(loom_hir::ConceptRef {
                    path: constructor,
                    bindings,
                })
            }
            _ => None,
        };
        if let Some(candidate) = candidate {
            let resolves_as_concept =
                crate::Resolver::new(self.program, self.def_maps, context.module)
                    .resolve_definition(&candidate.path, Namespace::Concept)
                    .is_ok();
            if resolves_as_concept {
                let ty = match self.resolve_concept_ref(&candidate, context, true) {
                    Some(target) => self.dynamic_view_type(target),
                    None => self.typed.types.error(),
                };
                return self.record_type(reference, ty);
            }
        }
        self.resolve_type_ref(reference, context)
    }

    fn resolve_type_ref(&mut self, reference: TypeRefId, context: &TypeContext) -> TyId {
        if let Some(ty) = self.typed.resolved_type_refs.get(reference).copied() {
            return ty;
        }
        let source = self.program.type_refs[reference].clone();
        let ty = match source {
            TypeRef::Error => self.typed.types.error(),
            TypeRef::Tuple(elements) => {
                let elements = elements
                    .into_iter()
                    .map(|element| self.resolve_type_ref(element, context))
                    .collect();
                self.typed.types.intern(TyData::Tuple(elements))
            }
            TypeRef::Path(path) => self.resolve_type_path(&path, context),
            TypeRef::Apply {
                constructor,
                arguments,
            } => self.resolve_type_application(&constructor, &arguments, context, reference),
            TypeRef::SelfType => context.self_ty.unwrap_or_else(|| {
                self.error(
                    "UnknownName",
                    "`Self` is only available in an impl or concept member",
                    self.type_span(reference),
                );
                self.typed.types.error()
            }),
            TypeRef::Projection {
                self_ty,
                concept,
                associated,
            } => {
                let self_ty = self.resolve_type_ref(self_ty, context);
                let concept = if let Some(concept_path) = concept {
                    let Some(concept) =
                        self.resolve_definition(&concept_path, Namespace::Concept, context.module)
                    else {
                        return self.record_type(reference, self.typed.types.error());
                    };
                    concept
                } else if let Some(concept) = context.self_concept {
                    concept
                } else {
                    self.error(
                        "AssociatedProjectionAmbiguous",
                        "associated projection does not identify a concept",
                        self.type_span(reference),
                    );
                    return self.record_type(reference, self.typed.types.error());
                };
                let Some(associated_type) = self.concept_associated_type(concept, &associated)
                else {
                    self.error(
                        "UnknownName",
                        format!("concept has no associated type `{associated}`"),
                        self.type_span(reference),
                    );
                    return self.record_type(reference, self.typed.types.error());
                };
                self.typed.types.intern(TyData::Projection {
                    self_ty,
                    concept,
                    associated_type,
                })
            }
            TypeRef::Dyn(target) => match self.resolve_concept_ref(&target, context, true) {
                Some(target) => self.dynamic_view_type(target),
                None => self.typed.types.error(),
            },
            TypeRef::View { .. } => {
                self.error(
                    "UnsupportedSyntax",
                    "`view[...]` has been replaced by the single `dyn C` type",
                    self.type_span(reference),
                );
                self.typed.types.error()
            }
            TypeRef::UnavailableCarrier { .. } => {
                self.error(
                    "UnsupportedSyntax",
                    "dynamic concepts use the single `dyn C` type",
                    self.type_span(reference),
                );
                self.typed.types.error()
            }
        };
        self.record_type(reference, ty)
    }

    fn dynamic_view_type(&mut self, target: ConceptInstance) -> TyId {
        let mutability = if self.concept_has_mutable_receiver(target.concept) {
            Mutability::Mutable
        } else {
            Mutability::ReadOnly
        };
        let target = self.typed.types.intern(TyData::DynTarget(target));
        self.typed.types.intern(TyData::View { mutability, target })
    }

    fn concept_has_mutable_receiver(&self, concept: DefId) -> bool {
        let DefinitionKind::Concept(concept) = &self.program.definitions[concept].kind else {
            return false;
        };
        concept.requirements.iter().any(|requirement| {
            matches!(
                &self.program.definitions[*requirement].kind,
                DefinitionKind::Method(method)
                    if method.signature.receiver == Some(ReceiverKind::Mutable)
            )
        })
    }

    fn resolve_type_application(
        &mut self,
        constructor: &Path,
        arguments: &[TypeArgumentRef],
        context: &TypeContext,
        reference: TypeRefId,
    ) -> TyId {
        let mut type_arguments = Vec::new();
        for argument in arguments {
            match argument {
                TypeArgumentRef::Type(ty) => {
                    type_arguments.push(self.resolve_type_ref(*ty, context));
                }
                TypeArgumentRef::Binding(_) => {
                    self.error(
                        "TypeMismatch",
                        "associated bindings are only valid on concept references",
                        self.type_span(reference),
                    );
                }
            }
        }
        if constructor.segments.len() == 1 {
            match constructor.segments[0].name.as_str() {
                "Option" => {
                    if type_arguments.len() != 1 {
                        self.report_arity(reference, "Option", 1, type_arguments.len());
                        return self.typed.types.error();
                    }
                    return self.typed.types.intern(TyData::Option(type_arguments[0]));
                }
                "Result" => {
                    if type_arguments.len() != 2 {
                        self.report_arity(reference, "Result", 2, type_arguments.len());
                        return self.typed.types.error();
                    }
                    return self.typed.types.intern(TyData::Result {
                        ok: type_arguments[0],
                        error: type_arguments[1],
                    });
                }
                "List" => {
                    if type_arguments.len() != 1 {
                        self.report_arity(reference, "List", 1, type_arguments.len());
                        return self.typed.types.error();
                    }
                    return self.typed.types.intern(TyData::List(type_arguments[0]));
                }
                "Task" => {
                    if type_arguments.len() != 1 {
                        self.report_arity(reference, "Task", 1, type_arguments.len());
                        return self.typed.types.error();
                    }
                    return self.typed.types.intern(TyData::Task(type_arguments[0]));
                }
                "TaskOutcome" => {
                    if type_arguments.len() != 1 {
                        self.report_arity(reference, "TaskOutcome", 1, type_arguments.len());
                        return self.typed.types.error();
                    }
                    return self
                        .typed
                        .types
                        .intern(TyData::TaskOutcome(type_arguments[0]));
                }
                _ => {}
            }
        }
        let Some(definition) =
            self.resolve_definition(constructor, Namespace::Type, context.module)
        else {
            return self.typed.types.error();
        };
        let expected = self.type_generic_params(definition);
        if expected.len() != type_arguments.len() {
            self.report_arity(
                reference,
                constructor.as_string(),
                expected.len(),
                type_arguments.len(),
            );
            return self.typed.types.error();
        }
        self.typed.types.intern(TyData::Nominal {
            definition,
            arguments: type_arguments,
        })
    }

    fn resolve_type_path(&mut self, path: &Path, context: &TypeContext) -> TyId {
        if path.segments.len() == 2 {
            let parameter_name = &path.segments[0].name;
            if let Some(parameter) = context.generic_params.get(parameter_name).copied() {
                let associated_name = &path.segments[1].name;
                let declared_bounds = self.program.generic_params[parameter].bounds.clone();
                let mut candidates = Vec::new();
                for bound in declared_bounds {
                    let Some(instance) = self.resolve_concept_ref(&bound, context, false) else {
                        continue;
                    };
                    if let Some(associated_type) =
                        self.concept_associated_type(instance.concept, associated_name)
                    {
                        candidates.push((instance, associated_type));
                    }
                }
                if candidates.len() == 1 {
                    let (instance, associated_type) = candidates.pop().expect("one candidate");
                    if let Some(binding) = instance
                        .bindings
                        .iter()
                        .find(|binding| binding.associated_type == associated_type)
                    {
                        return binding.ty;
                    }
                    let self_ty = self.typed.types.intern(TyData::Param(parameter));
                    return self.typed.types.intern(TyData::Projection {
                        self_ty,
                        concept: instance.concept,
                        associated_type,
                    });
                }
                self.error(
                    if candidates.is_empty() {
                        "UnknownName"
                    } else {
                        "AssociatedProjectionAmbiguous"
                    },
                    if candidates.is_empty() {
                        format!(
                            "no bound on `{parameter_name}` declares associated type `{associated_name}`"
                        )
                    } else {
                        format!(
                            "multiple bounds on `{parameter_name}` declare associated type `{associated_name}`"
                        )
                    },
                    path.segments[1].span,
                );
                return self.typed.types.error();
            }
        }
        if path.segments.len() == 1 {
            let name = &path.segments[0].name;
            if let Some(parameter) = context.generic_params.get(name) {
                return self.typed.types.intern(TyData::Param(*parameter));
            }
            if let Some(builtin) = builtin_type(name.as_str()) {
                return self.typed.types.builtin(builtin);
            }
            if matches!(
                name.as_str(),
                "Option" | "Result" | "List" | "Task" | "TaskOutcome"
            ) {
                self.error(
                    "TypeMismatch",
                    format!("generic type `{name}` requires type arguments"),
                    path.segments[0].span,
                );
                return self.typed.types.error();
            }
        }
        let Some(definition) = self.resolve_definition(path, Namespace::Type, context.module)
        else {
            return self.typed.types.error();
        };
        let generic_params = self.type_generic_params(definition);
        if !generic_params.is_empty() {
            self.error(
                "CannotInferType",
                format!(
                    "generic type `{}` requires type arguments",
                    path.as_string()
                ),
                path.segments
                    .last()
                    .map_or_else(Span::default, |segment| segment.span),
            );
            return self.typed.types.error();
        }
        self.typed.types.intern(TyData::Nominal {
            definition,
            arguments: Vec::new(),
        })
    }

    fn resolve_concept_ref(
        &mut self,
        source: &loom_hir::ConceptRef,
        context: &TypeContext,
        require_dynamic: bool,
    ) -> Option<ConceptInstance> {
        let concept = self.resolve_definition(&source.path, Namespace::Concept, context.module)?;
        let DefinitionKind::Concept(declaration) = &self.program.definitions[concept].kind else {
            return None;
        };
        if require_dynamic && !declaration.dyn_capable {
            self.error(
                "DynNotDeclared",
                format!(
                    "concept `{}` was not declared `dyn`",
                    source.path.as_string()
                ),
                source
                    .path
                    .segments
                    .last()
                    .map_or_else(Span::default, |segment| segment.span),
            );
        }
        let mut bindings = Vec::new();
        let mut seen = BTreeSet::new();
        for binding in &source.bindings {
            let Some(associated_type) = self.concept_associated_type(concept, &binding.name) else {
                self.error(
                    "UnknownName",
                    format!("concept has no associated type `{}`", binding.name),
                    source
                        .path
                        .segments
                        .last()
                        .map_or_else(Span::default, |segment| segment.span),
                );
                continue;
            };
            if !seen.insert(associated_type) {
                self.error(
                    "DuplicateDeclaration",
                    format!("associated type `{}` is bound more than once", binding.name),
                    source
                        .path
                        .segments
                        .last()
                        .map_or_else(Span::default, |segment| segment.span),
                );
                continue;
            }
            bindings.push(AssociatedTypeBinding {
                associated_type,
                ty: self.resolve_type_ref(binding.ty, context),
            });
        }
        bindings.sort_by_key(|binding| binding.associated_type);
        if require_dynamic {
            for associated in &declaration.associated_types {
                if !seen.contains(associated) {
                    let name = self.program.definitions[*associated]
                        .name
                        .as_ref()
                        .map_or("<error>", Name::as_str);
                    self.error(
                        "DynAssociatedTypeUnbound",
                        format!("dyn use must bind associated type `{name}`"),
                        source
                            .path
                            .segments
                            .last()
                            .map_or_else(Span::default, |segment| segment.span),
                    );
                }
            }
        }
        Some(ConceptInstance { concept, bindings })
    }

    fn type_context(&mut self, definition: DefId) -> TypeContext {
        let mut generic_params = BTreeMap::new();
        for parameter in self.generic_ids_for(definition) {
            generic_params.insert(
                self.program.generic_params[parameter].name.clone(),
                parameter,
            );
        }
        let self_ty = self.self_type_for(definition, &generic_params);
        let self_concept = self.self_concept_for(definition);
        TypeContext {
            module: self.program.definitions[definition].module,
            generic_params,
            self_ty,
            self_concept,
        }
    }

    fn self_type_for(
        &mut self,
        definition: DefId,
        generic_params: &BTreeMap<Name, GenericParamId>,
    ) -> Option<TyId> {
        match &self.program.definitions[definition].kind {
            DefinitionKind::Method(method) => {
                let owner = method.owner;
                match &self.program.definitions[owner].kind {
                    DefinitionKind::InherentImpl(implementation) => {
                        let context = TypeContext {
                            module: self.program.definitions[owner].module,
                            generic_params: generic_params.clone(),
                            self_ty: None,
                            self_concept: None,
                        };
                        Some(self.resolve_type_ref(implementation.target, &context))
                    }
                    DefinitionKind::Conformance(conformance) => {
                        let context = TypeContext {
                            module: self.program.definitions[owner].module,
                            generic_params: generic_params.clone(),
                            self_ty: None,
                            self_concept: None,
                        };
                        Some(self.resolve_type_ref(conformance.target, &context))
                    }
                    DefinitionKind::Concept(_) => {
                        Some(self.typed.types.intern(TyData::SelfType(owner)))
                    }
                    _ => None,
                }
            }
            DefinitionKind::AssociatedType(associated)
                if matches!(
                    self.program.definitions[associated.owner].kind,
                    DefinitionKind::Concept(_)
                ) =>
            {
                Some(self.typed.types.intern(TyData::SelfType(associated.owner)))
            }
            _ => None,
        }
    }

    fn self_concept_for(&self, definition: DefId) -> Option<DefId> {
        match &self.program.definitions[definition].kind {
            DefinitionKind::Method(method) => match &self.program.definitions[method.owner].kind {
                DefinitionKind::Concept(_) => Some(method.owner),
                DefinitionKind::Conformance(conformance) => {
                    let module = self.program.definitions[method.owner].module;
                    crate::Resolver::new(self.program, self.def_maps, module)
                        .resolve_definition(&conformance.concept.path, Namespace::Concept)
                        .ok()
                }
                _ => None,
            },
            DefinitionKind::AssociatedType(associated) => matches!(
                self.program.definitions[associated.owner].kind,
                DefinitionKind::Concept(_)
            )
            .then_some(associated.owner),
            _ => None,
        }
    }

    fn generic_ids_for(&self, definition: DefId) -> Vec<GenericParamId> {
        let mut parameters = Vec::new();
        self.collect_generic_ids(definition, &mut parameters);
        parameters
    }

    fn collect_generic_ids(&self, definition: DefId, output: &mut Vec<GenericParamId>) {
        match &self.program.definitions[definition].kind {
            DefinitionKind::Record(record) => output.extend(&record.generic_params),
            DefinitionKind::Enum(enumeration) => output.extend(&enumeration.generic_params),
            DefinitionKind::Function(function) | DefinitionKind::Test(function) => {
                output.extend(&function.signature.generic_params);
            }
            DefinitionKind::InherentImpl(implementation) => {
                output.extend(&implementation.generic_params);
            }
            DefinitionKind::Conformance(conformance) => {
                output.extend(&conformance.generic_params);
            }
            DefinitionKind::Method(method) => {
                self.collect_generic_ids(method.owner, output);
                output.extend(&method.signature.generic_params);
            }
            DefinitionKind::Field(field) => self.collect_generic_ids(field.owner, output),
            DefinitionKind::Variant(variant) => self.collect_generic_ids(variant.owner, output),
            DefinitionKind::AssociatedType(associated) => {
                self.collect_generic_ids(associated.owner, output);
            }
            DefinitionKind::Error | DefinitionKind::RefinedType(_) | DefinitionKind::Concept(_) => {
            }
        }
    }

    fn type_generic_params(&self, definition: DefId) -> Vec<GenericParamId> {
        match &self.program.definitions[definition].kind {
            DefinitionKind::Record(record) => record.generic_params.clone(),
            DefinitionKind::Enum(enumeration) => enumeration.generic_params.clone(),
            _ => Vec::new(),
        }
    }

    fn nominal_self_type(&mut self, definition: DefId) -> TyId {
        let arguments = self
            .type_generic_params(definition)
            .into_iter()
            .map(|parameter| self.typed.types.intern(TyData::Param(parameter)))
            .collect();
        self.typed.types.intern(TyData::Nominal {
            definition,
            arguments,
        })
    }

    fn resolve_definition(
        &mut self,
        path: &Path,
        namespace: Namespace,
        module: ModuleId,
    ) -> Option<DefId> {
        let result = crate::Resolver::new(self.program, self.def_maps, module)
            .resolve_definition(path, namespace);
        match result {
            Ok(definition) => Some(definition),
            Err(error) => {
                let span = path
                    .segments
                    .last()
                    .map_or_else(Span::default, |segment| segment.span);
                let (code, message) = match error {
                    ResolveError::Missing => (
                        "UnknownName",
                        format!("unknown name `{}`", path.as_string()),
                    ),
                    ResolveError::Duplicate(_) => (
                        "DuplicateDeclaration",
                        format!("`{}` has multiple declarations", path.as_string()),
                    ),
                    ResolveError::Private(_) => (
                        "NameNotVisible",
                        format!("`{}` is not visible here", path.as_string()),
                    ),
                    ResolveError::UnknownModule(module) => {
                        ("UnknownName", format!("unknown module `{module}`"))
                    }
                };
                self.error(code, message, span);
                None
            }
        }
    }

    fn concept_associated_type(&self, concept: DefId, name: &Name) -> Option<DefId> {
        let DefinitionKind::Concept(concept) = &self.program.definitions[concept].kind else {
            return None;
        };
        concept
            .associated_types
            .iter()
            .copied()
            .find(|associated| self.program.definitions[*associated].name.as_ref() == Some(name))
    }

    fn validate_generic_params(&mut self, parameters: &[GenericParamId]) {
        let mut names = BTreeMap::<Name, GenericParamId>::new();
        for parameter in parameters {
            let source = &self.program.generic_params[*parameter];
            if let Some(previous) = names.insert(source.name.clone(), *parameter) {
                self.diagnostics.push(
                    Diagnostic::error(
                        "DuplicateDeclaration",
                        format!(
                            "generic parameter `{}` is declared more than once",
                            source.name
                        ),
                        self.generic_span(*parameter),
                    )
                    .with_label(self.generic_span(previous), "first declaration"),
                );
            }
        }
    }

    fn validate_unique_members<'a>(
        &mut self,
        _owner: DefId,
        members: impl Iterator<Item = &'a DefId>,
    ) {
        let mut names = BTreeMap::new();
        for member in members {
            let Some(name) = self.program.definitions[*member].name.clone() else {
                continue;
            };
            if let Some(previous) = names.insert(name.clone(), *member) {
                self.diagnostics.push(
                    Diagnostic::error(
                        "DuplicateDeclaration",
                        format!("member `{name}` is declared more than once"),
                        self.definition_span(*member),
                    )
                    .with_label(self.definition_span(previous), "first declaration"),
                );
            }
        }
    }

    fn validate_test_signature(&mut self, definition: DefId, signature: &CallableSignature) {
        let unit = self.typed.types.builtin(BuiltinType::Unit);
        let valid_return = signature.return_ty == unit
            || matches!(
                self.typed.types.data(signature.return_ty),
                TyData::Result { ok, .. } if *ok == unit
            );
        if !signature.generic_params.is_empty()
            || signature.receiver.is_some()
            || !signature.params.is_empty()
            || !valid_return
        {
            self.error(
                "InvalidTestSignature",
                "test fn must have no parameters or generics and return Unit or Result[Unit, E]",
                self.definition_span(definition),
            );
        }
    }

    fn validate_dynamic_concepts(&mut self) {
        for (concept_id, definition) in self.program.definitions.iter() {
            let DefinitionKind::Concept(concept) = &definition.kind else {
                continue;
            };
            if !concept.dyn_capable {
                continue;
            }
            for requirement in &concept.requirements {
                let DefinitionKind::Method(method) = &self.program.definitions[*requirement].kind
                else {
                    continue;
                };
                if method.signature.receiver == Some(ReceiverKind::Static) {
                    self.error(
                        "DynStaticRequirement",
                        "dyn concept cannot contain a static method",
                        self.definition_span(*requirement),
                    );
                }
                if !method.signature.generic_params.is_empty() {
                    self.error(
                        "DynGenericMethod",
                        "dyn concept method cannot have method-specific type parameters",
                        self.definition_span(*requirement),
                    );
                }
                self.validate_dyn_self_uses(concept_id, *requirement, &method.signature);
            }
        }
    }

    fn build_conformances(&mut self) {
        let conformances = self
            .program
            .definitions
            .iter()
            .filter_map(|(definition, item)| {
                matches!(item.kind, DefinitionKind::Conformance(_)).then_some(definition)
            })
            .collect::<Vec<_>>();
        for definition in conformances {
            self.build_conformance(definition);
        }
        self.impl_index.finish();
        for (left, right) in self.impl_index.overlapping_pairs(&self.typed.types) {
            let left_target = self.conformance_target(left);
            let right_target = self.conformance_target(right);
            let duplicate = left_target.is_some() && left_target == right_target;
            self.diagnostics.push(
                Diagnostic::error(
                    if duplicate {
                        "DuplicateConformance"
                    } else {
                        "OverlappingConformance"
                    },
                    if duplicate {
                        "the same conformance is declared more than once"
                    } else {
                        "conformance target heads may overlap"
                    },
                    self.definition_span(right),
                )
                .with_label(self.definition_span(left), "conflicting conformance"),
            );
        }
    }

    #[allow(clippy::too_many_lines)]
    fn build_conformance(&mut self, definition: DefId) {
        let DefinitionKind::Conformance(source) = self.program.definitions[definition].kind.clone()
        else {
            return;
        };
        let context = self.type_context(definition);
        let target = self.resolve_type_ref(source.target, &context);
        let Some(mut concept) = self.resolve_concept_ref(&source.concept, &context, false) else {
            return;
        };
        let concept_definition = concept.concept;
        let DefinitionKind::Concept(concept_decl) =
            self.program.definitions[concept_definition].kind.clone()
        else {
            return;
        };
        let mut valid = true;
        let target_owner = match self.typed.types.data(target) {
            TyData::Nominal { definition, .. } => Some(*definition),
            _ => None,
        };
        let valid_target_head = matches!(
            self.typed.types.data(target),
            TyData::Nominal { .. } | TyData::Builtin(_) | TyData::Option(_) | TyData::Result { .. }
        );
        if !valid_target_head {
            self.error(
                "UnconstrainedImplParameter",
                "conformance target must have a concrete builtin, prelude, or nominal outer constructor",
                self.definition_span(definition),
            );
            valid = false;
        }
        let module = self.program.definitions[definition].module;
        let owns_concept = self.program.definitions[concept_definition].module == module;
        let owns_target =
            target_owner.is_some_and(|owner| self.program.definitions[owner].module == module);
        if !owns_concept && !owns_target {
            self.error(
                "ForeignConformance",
                "conformance must be declared by the concept or target type owner module",
                self.definition_span(definition),
            );
            valid = false;
        }
        let target_parameters = type_parameters_in(&self.typed.types, target);
        for parameter in &source.generic_params {
            if !target_parameters.contains(parameter) {
                self.error(
                    "UnconstrainedImplParameter",
                    format!(
                        "impl parameter `{}` is not determined by the target head",
                        self.program.generic_params[*parameter].name
                    ),
                    self.generic_span(*parameter),
                );
                valid = false;
            }
        }

        let mut associated_types = Vec::new();
        let mut seen_associated = BTreeSet::new();
        for binding_definition in &source.associated_types {
            let Some(name) = self.program.definitions[*binding_definition].name.as_ref() else {
                continue;
            };
            let Some(requirement) = concept_decl
                .associated_types
                .iter()
                .copied()
                .find(|candidate| self.program.definitions[*candidate].name.as_ref() == Some(name))
            else {
                self.error(
                    "IncompleteConformance",
                    format!("concept has no associated type `{name}`"),
                    self.definition_span(*binding_definition),
                );
                valid = false;
                continue;
            };
            if !seen_associated.insert(requirement) {
                self.error(
                    "DuplicateDeclaration",
                    format!("associated type `{name}` is bound more than once"),
                    self.definition_span(*binding_definition),
                );
                valid = false;
                continue;
            }
            let DefinitionKind::AssociatedType(binding) =
                &self.program.definitions[*binding_definition].kind
            else {
                continue;
            };
            let Some(value) = binding.binding else {
                continue;
            };
            associated_types.push(AssociatedTypeBinding {
                associated_type: requirement,
                ty: self.resolve_type_ref(value, &context),
            });
        }
        for requirement in &concept_decl.associated_types {
            if !seen_associated.contains(requirement) {
                let name = self.program.definitions[*requirement]
                    .name
                    .as_ref()
                    .map_or("<error>", Name::as_str);
                self.error(
                    "IncompleteConformance",
                    format!("missing associated type `{name}`"),
                    self.definition_span(definition),
                );
                valid = false;
            }
        }
        associated_types.sort_by_key(|binding| binding.associated_type);
        concept.bindings.clone_from(&associated_types);

        let mut methods = BTreeMap::new();
        for method in &source.methods {
            let Some(name) = self.program.definitions[*method].name.as_ref() else {
                continue;
            };
            let Some(requirement) =
                concept_decl.requirements.iter().copied().find(|candidate| {
                    self.program.definitions[*candidate].name.as_ref() == Some(name)
                })
            else {
                self.error(
                    "IncompleteConformance",
                    format!("concept has no method `{name}`"),
                    self.definition_span(*method),
                );
                valid = false;
                continue;
            };
            if methods.insert(requirement, *method).is_some() {
                self.error(
                    "DuplicateDeclaration",
                    format!("method `{name}` is implemented more than once"),
                    self.definition_span(*method),
                );
                valid = false;
            }
        }
        for requirement in &concept_decl.requirements {
            if !methods.contains_key(requirement) {
                let name = self.program.definitions[*requirement]
                    .name
                    .as_ref()
                    .map_or("<error>", Name::as_str);
                self.error(
                    "IncompleteConformance",
                    format!("missing method `{name}`"),
                    self.definition_span(definition),
                );
                valid = false;
            }
        }
        for (requirement, implementation) in &methods {
            if !self.conformance_signatures_match(*requirement, *implementation, target, &concept) {
                self.diagnostics.push(
                    Diagnostic::error(
                        "ConformanceSignatureMismatch",
                        "implementation method does not match its concept requirement",
                        self.definition_span(*implementation),
                    )
                    .with_label(self.definition_span(*requirement), "required signature"),
                );
                valid = false;
            }
        }

        let mut conditions = Vec::new();
        for parameter in &source.generic_params {
            let parameter_ty = self.typed.types.intern(TyData::Param(*parameter));
            for bound in &self.program.generic_params[*parameter].bounds {
                if let Some(bound) = self.resolve_concept_ref(bound, &context, false) {
                    if !is_strict_structural_subterm(&self.typed.types, parameter_ty, target) {
                        self.error(
                            "ConformanceResolutionCycle",
                            "conformance prerequisite must apply to a strict structural subterm",
                            self.generic_span(*parameter),
                        );
                        valid = false;
                    }
                    conditions.push(Goal {
                        self_ty: parameter_ty,
                        concept: bound,
                    });
                }
            }
        }
        self.typed.conformances.insert(
            definition,
            crate::ConformanceSemantics {
                concept: concept.clone(),
                target,
                methods,
                associated_types: associated_types.clone(),
            },
        );
        if valid {
            self.impl_index.insert(ImplHeader {
                definition,
                generic_params: source.generic_params,
                concept: concept_definition,
                target,
                conditions,
                associated_types,
            });
        }
    }

    fn conformance_signatures_match(
        &mut self,
        requirement: DefId,
        implementation: DefId,
        target: TyId,
        concept: &ConceptInstance,
    ) -> bool {
        let Some(Signature::Callable(required)) = self.typed.signatures.get(requirement).cloned()
        else {
            return false;
        };
        let Some(Signature::Callable(actual)) = self.typed.signatures.get(implementation).cloned()
        else {
            return false;
        };
        if required.receiver != actual.receiver
            || required.params.len() != actual.params.len()
            || required.call_generic_params.len() != actual.call_generic_params.len()
        {
            return false;
        }
        let mut alpha = Substitution::default();
        for (required, actual) in required
            .call_generic_params
            .iter()
            .zip(&actual.call_generic_params)
        {
            alpha.insert(*required, self.typed.types.intern(TyData::Param(*actual)));
        }
        let required_params = required.params.into_iter().map(|(_, ty)| {
            let ty = self.instantiate_concept_type(ty, target, concept);
            self.typed.types.substitute(ty, &alpha)
        });
        if !required_params
            .zip(actual.params.iter().map(|(_, ty)| *ty))
            .all(|(required, actual)| required == actual)
        {
            return false;
        }
        let required_return = self.instantiate_concept_type(required.return_ty, target, concept);
        let required_return = self.typed.types.substitute(required_return, &alpha);
        let contracts_empty = match &self.program.definitions[implementation].kind {
            DefinitionKind::Method(method) => {
                method.signature.contracts.requires.is_empty()
                    && method.signature.contracts.ensures.is_empty()
            }
            _ => false,
        };
        required_return == actual.return_ty
            && self.conformance_method_bounds_match(
                &required.call_bounds,
                &actual.call_bounds,
                target,
                concept,
                &alpha,
            )
            && contracts_empty
    }

    fn instantiate_concept_type(
        &mut self,
        ty: TyId,
        concrete_self: TyId,
        instance: &ConceptInstance,
    ) -> TyId {
        match self.typed.types.data(ty).clone() {
            TyData::SelfType(concept) if concept == instance.concept => concrete_self,
            TyData::Projection {
                concept,
                associated_type,
                ..
            } if concept == instance.concept => {
                if let Some(binding) = instance
                    .bindings
                    .iter()
                    .find(|binding| binding.associated_type == associated_type)
                {
                    binding.ty
                } else {
                    self.typed.types.intern(TyData::Projection {
                        self_ty: concrete_self,
                        concept,
                        associated_type,
                    })
                }
            }
            TyData::Option(element) => {
                let element = self.instantiate_concept_type(element, concrete_self, instance);
                self.typed.types.intern(TyData::Option(element))
            }
            TyData::Result { ok, error } => {
                let ok = self.instantiate_concept_type(ok, concrete_self, instance);
                let error = self.instantiate_concept_type(error, concrete_self, instance);
                self.typed.types.intern(TyData::Result { ok, error })
            }
            TyData::Nominal {
                definition,
                arguments,
            } => {
                let arguments = arguments
                    .into_iter()
                    .map(|argument| {
                        self.instantiate_concept_type(argument, concrete_self, instance)
                    })
                    .collect();
                self.typed.types.intern(TyData::Nominal {
                    definition,
                    arguments,
                })
            }
            _ => ty,
        }
    }

    fn conformance_method_bounds_match(
        &mut self,
        required: &[Bound],
        actual: &[Bound],
        target: TyId,
        concept: &ConceptInstance,
        alpha: &Substitution,
    ) -> bool {
        let mut required = required
            .iter()
            .map(|bound| {
                let self_ty = self.instantiate_concept_type(bound.self_ty, target, concept);
                let self_ty = self.typed.types.substitute(self_ty, alpha);
                let bindings = bound
                    .concept
                    .bindings
                    .iter()
                    .map(|binding| {
                        let ty = self.instantiate_concept_type(binding.ty, target, concept);
                        (
                            binding.associated_type,
                            self.typed.types.substitute(ty, alpha),
                        )
                    })
                    .collect::<Vec<_>>();
                (self_ty, bound.concept.concept, bindings)
            })
            .collect::<Vec<_>>();
        let mut actual = actual
            .iter()
            .map(|bound| {
                (
                    bound.self_ty,
                    bound.concept.concept,
                    bound
                        .concept
                        .bindings
                        .iter()
                        .map(|binding| (binding.associated_type, binding.ty))
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();
        required.sort_unstable();
        actual.sort_unstable();
        required == actual
    }

    fn conformance_target(&self, definition: DefId) -> Option<TyId> {
        self.typed
            .conformances
            .get(definition)
            .map(|conformance| conformance.target)
    }

    fn validate_dyn_self_uses(
        &mut self,
        concept: DefId,
        requirement: DefId,
        signature: &loom_hir::CallableSignature,
    ) {
        for parameter in &signature.params {
            if self.type_ref_leaks_self(self.program.params[*parameter].ty, concept) {
                self.error(
                    "DynSelfLeak",
                    "dyn method may use Self only as its receiver or in associated projections",
                    self.definition_span(requirement),
                );
            }
        }
        if signature
            .return_ty
            .is_some_and(|ty| self.type_ref_leaks_self(ty, concept))
        {
            self.error(
                "DynSelfLeak",
                "dyn method may not return Self outside an associated projection",
                self.definition_span(requirement),
            );
        }
    }

    fn type_ref_leaks_self(&self, reference: TypeRefId, concept: DefId) -> bool {
        match &self.program.type_refs[reference] {
            TypeRef::SelfType => true,
            TypeRef::Tuple(elements) => elements
                .iter()
                .any(|element| self.type_ref_leaks_self(*element, concept)),
            TypeRef::Apply { arguments, .. } => arguments.iter().any(|argument| match argument {
                TypeArgumentRef::Type(ty) => self.type_ref_leaks_self(*ty, concept),
                TypeArgumentRef::Binding(binding) => self.type_ref_leaks_self(binding.ty, concept),
            }),
            TypeRef::Projection {
                concept: projection_concept,
                ..
            } => projection_concept
                .as_ref()
                .is_some_and(|path| self.program.definitions[concept].name.as_ref() != path.last()),
            TypeRef::View { target, .. } => self.type_ref_leaks_self(*target, concept),
            TypeRef::Dyn(target) | TypeRef::UnavailableCarrier { target, .. } => target
                .bindings
                .iter()
                .any(|binding| self.type_ref_leaks_self(binding.ty, concept)),
            TypeRef::Error | TypeRef::Path(_) => false,
        }
    }

    fn check_bodies(&mut self) {
        // Body checking is implemented below in this module; keeping this
        // separate from signature collection guarantees definition-site generic
        // checking and order-independent call resolution.
        let bodies = self
            .program
            .bodies
            .iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        for body in bodies {
            self.check_body(body);
        }
    }

    fn check_body(&mut self, body: BodyId) {
        let environment = self.body_environment(body);
        let mut checker = BodyChecker::new(self, body, environment);
        checker.check();
        let semantics = checker.semantics;
        self.typed.bodies.insert(body, semantics);
    }

    #[allow(clippy::too_many_lines)]
    fn body_environment(&mut self, body: BodyId) -> BodyEnvironment {
        let source = &self.program.bodies[body];
        let owner = source.owner;
        let bool_ty = self.typed.types.builtin(BuiltinType::Bool);
        let unit_ty = self.typed.types.builtin(BuiltinType::Unit);
        let signature = match self.typed.signatures.get(owner) {
            Some(Signature::Callable(signature)) => Some(signature.clone()),
            _ => None,
        };
        let context = self.type_context(owner);
        let (expected_root, return_ty, self_ty, receiver, result_ty, contract) = match source.kind {
            BodyKind::Function | BodyKind::Method => {
                let return_ty = signature
                    .as_ref()
                    .map_or(unit_ty, |signature| signature.return_ty);
                (
                    return_ty,
                    return_ty,
                    context.self_ty,
                    signature.as_ref().and_then(|signature| signature.receiver),
                    None,
                    ContractMode::None,
                )
            }
            BodyKind::RefinementPredicate => {
                let self_ty = match &self.program.definitions[owner].kind {
                    DefinitionKind::RefinedType(refined) => self
                        .typed
                        .resolved_type_refs
                        .get(refined.base)
                        .copied()
                        .unwrap_or_else(|| self.typed.types.error()),
                    _ => self.typed.types.error(),
                };
                (
                    bool_ty,
                    bool_ty,
                    Some(self_ty),
                    Some(ReceiverKind::ReadOnly),
                    None,
                    ContractMode::Predicate { old: false },
                )
            }
            BodyKind::RecordInvariant => {
                let self_ty = self.nominal_self_type(owner);
                (
                    bool_ty,
                    bool_ty,
                    Some(self_ty),
                    Some(ReceiverKind::ReadOnly),
                    None,
                    ContractMode::Predicate { old: false },
                )
            }
            BodyKind::Requires => (
                bool_ty,
                bool_ty,
                context.self_ty,
                signature.as_ref().and_then(|signature| signature.receiver),
                None,
                ContractMode::Predicate { old: false },
            ),
            BodyKind::Ensures => {
                let result = signature.as_ref().map(|signature| signature.return_ty);
                (
                    bool_ty,
                    bool_ty,
                    context.self_ty,
                    signature.as_ref().and_then(|signature| signature.receiver),
                    result,
                    ContractMode::Predicate { old: true },
                )
            }
        };
        let params = signature
            .as_ref()
            .map(|signature| {
                signature
                    .params
                    .iter()
                    .map(|(parameter, ty)| {
                        (
                            self.program.params[*parameter].name.clone(),
                            (*parameter, *ty),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let bounds = signature
            .as_ref()
            .map_or_else(Vec::new, |signature| signature.bounds.clone());
        BodyEnvironment {
            owner,
            expected_root,
            return_ty,
            self_ty,
            receiver,
            result_ty,
            params,
            bounds,
            contract,
            is_async: signature
                .as_ref()
                .is_some_and(|signature| signature.is_async),
        }
    }

    fn report_arity(
        &mut self,
        reference: TypeRefId,
        name: impl std::fmt::Display,
        expected: usize,
        actual: usize,
    ) {
        self.error(
            "TypeMismatch",
            format!("`{name}` expects {expected} type arguments, found {actual}"),
            self.type_span(reference),
        );
    }

    fn record_type(&mut self, reference: TypeRefId, ty: TyId) -> TyId {
        self.typed.resolved_type_refs.insert(reference, ty);
        ty
    }

    fn type_span(&self, reference: TypeRefId) -> Span {
        self.program
            .source_map
            .type_ref(reference)
            .unwrap_or_default()
    }

    fn generic_span(&self, parameter: GenericParamId) -> Span {
        self.program
            .source_map
            .generic_param(parameter)
            .unwrap_or_default()
    }

    fn definition_span(&self, definition: DefId) -> Span {
        self.program
            .source_map
            .definition(definition)
            .unwrap_or_default()
    }

    fn error(&mut self, code: &'static str, message: impl Into<String>, primary: Span) {
        self.diagnostics
            .push(Diagnostic::error(code, message, primary));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ContractMode {
    None,
    Predicate { old: bool },
}

#[derive(Clone, Debug)]
struct BodyEnvironment {
    owner: DefId,
    expected_root: TyId,
    return_ty: TyId,
    self_ty: Option<TyId>,
    receiver: Option<ReceiverKind>,
    result_ty: Option<TyId>,
    params: BTreeMap<Name, (ParamId, TyId)>,
    bounds: Vec<Bound>,
    contract: ContractMode,
    is_async: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExpressionContext {
    Value,
    UnitStatement,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PlaceAccess {
    Read,
    Write,
}

#[derive(Clone, Debug)]
struct ActiveBorrow {
    owner: Place,
    mutable: bool,
    region: RegionId,
    token: ViewTokenId,
    span: Span,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DynamicCoercionMode {
    Owned,
    CallBorrow,
}

#[derive(Clone)]
struct FlowState {
    self_dirty: bool,
    borrows: Vec<ActiveBorrow>,
}

#[allow(clippy::struct_excessive_bools)]
struct BodyChecker<'a, 'program> {
    analyzer: &'a mut Analyzer<'program>,
    body: BodyId,
    environment: BodyEnvironment,
    semantics: BodySemantics,
    scopes: Vec<BTreeMap<Name, LocalId>>,
    self_dirty: bool,
    allow_dirty_self_projection: bool,
    checking_assignment_target: bool,
    checking_view_source: bool,
    dynamic_coercion_mode: DynamicCoercionMode,
    regions: Vec<RegionId>,
    next_region: u32,
    next_view_token: u32,
    borrows: Vec<ActiveBorrow>,
    scoped_locals: BTreeSet<LocalId>,
    active_no_suspend: Vec<(LocalId, RegionId, Span)>,
    task_local_uses: BTreeSet<LocalId>,
    cleanup_depth: u32,
    allow_await_here: bool,
    checking_scoped_receiver: bool,
}

impl<'a, 'program> BodyChecker<'a, 'program> {
    fn new(
        analyzer: &'a mut Analyzer<'program>,
        body: BodyId,
        environment: BodyEnvironment,
    ) -> Self {
        Self {
            analyzer,
            body,
            environment,
            semantics: BodySemantics::default(),
            scopes: vec![BTreeMap::new()],
            self_dirty: false,
            allow_dirty_self_projection: false,
            checking_assignment_target: false,
            checking_view_source: false,
            dynamic_coercion_mode: DynamicCoercionMode::Owned,
            regions: vec![RegionId(0)],
            next_region: 1,
            next_view_token: 0,
            borrows: Vec::new(),
            scoped_locals: BTreeSet::new(),
            active_no_suspend: Vec::new(),
            task_local_uses: BTreeSet::new(),
            cleanup_depth: 0,
            allow_await_here: false,
            checking_scoped_receiver: false,
        }
    }

    fn check(&mut self) {
        let root = self.source().root;
        self.check_expr(
            root,
            Some(self.environment.expected_root),
            ExpressionContext::Value,
        );
    }

    fn check_expr(
        &mut self,
        expression: ExprId,
        expected: Option<TyId>,
        context: ExpressionContext,
    ) -> TyId {
        let await_allowed = self.allow_await_here;
        let source = self.source().expressions[expression].clone();
        self.validate_contract_shape(expression, &source);
        let inferred = match source {
            Expr::Error => self.types().error(),
            Expr::Literal(literal) => self.check_literal(expression, &literal),
            Expr::Tuple(elements) => self.check_tuple(&elements, expected),
            Expr::List(elements) => self.check_list(expression, &elements, expected),
            Expr::Path(path) => self.check_path(expression, &path, expected),
            Expr::SelfValue => self.check_self(expression),
            Expr::ResultValue => self.check_result(expression),
            Expr::Old(value) => self.check_old(expression, value),
            Expr::Block { statements, tail } => {
                self.check_block(&statements, tail, expected, context)
            }
            Expr::If {
                condition,
                then_branch,
                else_branch,
            } => self.check_if(condition, then_branch, else_branch, expected, context),
            Expr::Match { scrutinee, arms } => {
                self.check_match(scrutinee, &arms, expected, context)
            }
            Expr::Call {
                callee,
                type_arguments,
                arguments,
            } => self.check_call(expression, callee, &type_arguments, &arguments, expected),
            Expr::MethodCall {
                receiver,
                method,
                type_arguments,
                arguments,
            } => self.check_method_call(
                expression,
                receiver,
                &method,
                &type_arguments,
                &arguments,
                expected,
            ),
            Expr::QualifiedMethodCall {
                self_ty,
                concept,
                method,
                type_arguments,
                arguments,
            } => self.check_qualified_call(
                expression,
                self_ty,
                &concept,
                &method,
                &type_arguments,
                &arguments,
                expected,
            ),
            Expr::Field { receiver, name } => self.check_field(expression, receiver, &name),
            Expr::Unary { op, operand } => self.check_unary(expression, op, operand),
            Expr::Binary { op, left, right } => self.check_binary(expression, op, left, right),
            Expr::Assign { target, value } => self.check_assignment(expression, target, value),
            Expr::RecordLiteral { ty, fields } => {
                self.check_record_literal(expression, &ty, &fields, expected)
            }
            Expr::View {
                mutable,
                concept,
                source,
            } => self.check_view(expression, mutable, &concept, source),
            Expr::Await(value) => self.check_await(expression, value, await_allowed),
            Expr::Sleep(arguments) => self.check_sleep(expression, &arguments),
            Expr::WaitFd { arguments, .. } => self.check_wait_fd(expression, &arguments),
            Expr::TaskJoin { mode, arguments } => {
                self.check_task_join(expression, mode, &arguments)
            }
            Expr::Propagate(value) => self.check_propagate(expression, value),
            Expr::Return(value) => self.check_return(value),
        };
        let result = if let Some(expected) = expected {
            self.coerce(expression, inferred, expected)
        } else {
            inferred
        };
        self.semantics.expression_types.insert(expression, result);
        result
    }

    fn check_suspendable_expr(
        &mut self,
        expression: ExprId,
        expected: Option<TyId>,
        context: ExpressionContext,
    ) -> TyId {
        let previous = self.allow_await_here;
        self.allow_await_here = true;
        let result = self.check_expr(expression, expected, context);
        self.allow_await_here = previous;
        result
    }

    fn check_await(&mut self, expression: ExprId, value: ExprId, allowed: bool) -> TyId {
        if !self.environment.is_async {
            self.error_at(
                "AwaitOutsideAsync",
                "`await` is only valid inside an async function or async test",
                expression,
            );
        }
        if !allowed {
            self.error_at(
                "AwaitMustBeStatementBoundary",
                "the current executable async slice requires `await` directly in a binding, return, statement, or block tail",
                expression,
            );
        }
        if self.cleanup_depth > 0 {
            self.error_at(
                "AwaitInCleanup",
                "a defer cleanup must complete synchronously and cannot await",
                expression,
            );
        }
        if !self.borrows.is_empty() {
            self.error_at(
                "AccessAcrossAwait",
                "an active interface access cannot cross an await point",
                expression,
            );
        }
        for (_, _, span) in self.active_no_suspend.clone() {
            self.analyzer.diagnostics.push(
                Diagnostic::error(
                    "NoSuspendAcrossAwait",
                    "a NoSuspend scoped resource is still active at this await point",
                    self.expr_span(expression),
                )
                .with_label(span, "resource scope begins here"),
            );
        }
        let task = self.check_expr(value, None, ExpressionContext::Value);
        match self.types().data(task).clone() {
            TyData::Task(output) => output,
            TyData::Tuple(tasks) => {
                let mut outputs = Vec::with_capacity(tasks.len());
                let mut valid = true;
                for task in tasks {
                    if let TyData::Task(output) = self.types().data(task).clone() {
                        outputs.push(output);
                    } else {
                        valid = false;
                        outputs.push(self.types().error());
                    }
                }
                if !valid {
                    self.error_at(
                        "AwaitRequiresAsyncCall",
                        "every element of an awaited tuple must be an async task",
                        expression,
                    );
                }
                self.types().intern(TyData::Tuple(outputs))
            }
            TyData::Error => self.types().error(),
            _ => {
                self.error_at(
                    "AwaitRequiresAsyncCall",
                    "`await` operand does not produce a supported async task",
                    expression,
                );
                self.types().error()
            }
        }
    }

    fn check_sleep(&mut self, expression: ExprId, arguments: &[ExprId]) -> TyId {
        if arguments.len() != 1 {
            self.call_arity(expression, 1, arguments.len());
        }
        if let Some(argument) = arguments.first() {
            let actual = self.check_expr(*argument, None, ExpressionContext::Value);
            if !matches!(
                self.types().data(actual),
                TyData::Builtin(BuiltinType::Int | BuiltinType::Duration)
            ) {
                self.error_at(
                    "TypeMismatch",
                    "Task.sleep expects Int milliseconds or Duration",
                    *argument,
                );
            }
        }
        self.finish_call_arguments(arguments);
        let unit = self.types().builtin(BuiltinType::Unit);
        self.types().intern(TyData::Task(unit))
    }

    fn check_wait_fd(&mut self, expression: ExprId, arguments: &[ExprId]) -> TyId {
        let int = self.types().builtin(BuiltinType::Int);
        self.check_fixed_arguments(expression, arguments, &[int]);
        let unit = self.types().builtin(BuiltinType::Unit);
        self.types().intern(TyData::Task(unit))
    }

    fn check_task_join(
        &mut self,
        expression: ExprId,
        mode: TaskJoinMode,
        arguments: &[ExprId],
    ) -> TyId {
        if arguments.is_empty() {
            self.error_at(
                "EmptyStaticTaskJoin",
                "fixed Task joins require at least one task; use a typed empty List for all/settled",
                expression,
            );
            return self.types().error();
        }
        let argument_types = arguments
            .iter()
            .map(|argument| self.check_expr(*argument, None, ExpressionContext::Value))
            .collect::<Vec<_>>();

        if let [argument] = argument_types.as_slice()
            && let TyData::List(element) = self.types().data(*argument).clone()
        {
            let TyData::Task(output) = self.types().data(element).clone() else {
                self.error_at(
                    "TaskJoinRequiresTasks",
                    "a dynamic Task join requires List[Task[T]]",
                    expression,
                );
                return self.types().error();
            };
            let output = match mode {
                TaskJoinMode::All => {
                    let list = self.types().intern(TyData::List(output));
                    self.types().intern(TyData::Task(list))
                }
                TaskJoinMode::Settled => {
                    let outcome = self.types().intern(TyData::TaskOutcome(output));
                    let list = self.types().intern(TyData::List(outcome));
                    self.types().intern(TyData::Task(list))
                }
                TaskJoinMode::Any => self.types().intern(TyData::Task(output)),
                TaskJoinMode::Race => {
                    let outcome = self.types().intern(TyData::TaskOutcome(output));
                    self.types().intern(TyData::Task(outcome))
                }
            };
            return output;
        }

        let mut outputs = Vec::with_capacity(argument_types.len());
        for argument in argument_types {
            if let TyData::Task(output) = self.types().data(argument).clone() {
                outputs.push(output);
            } else {
                self.error_at(
                    "TaskJoinRequiresTasks",
                    "every fixed Task join argument must have Task[T] type",
                    expression,
                );
                outputs.push(self.types().error());
            }
        }
        let logical_output = match mode {
            TaskJoinMode::All => self.types().intern(TyData::Tuple(outputs)),
            TaskJoinMode::Settled => {
                let outcomes = outputs
                    .into_iter()
                    .map(|output| self.types().intern(TyData::TaskOutcome(output)))
                    .collect();
                self.types().intern(TyData::Tuple(outcomes))
            }
            TaskJoinMode::Any | TaskJoinMode::Race => {
                let first = outputs[0];
                if outputs.iter().any(|output| *output != first) {
                    self.error_at(
                        "HeterogeneousFirstTaskJoin",
                        "Task.any and Task.race require one common result type",
                        expression,
                    );
                }
                if mode == TaskJoinMode::Race {
                    self.types().intern(TyData::TaskOutcome(first))
                } else {
                    first
                }
            }
        };
        self.types().intern(TyData::Task(logical_output))
    }

    fn check_propagate(&mut self, expression: ExprId, value: ExprId) -> TyId {
        if self.cleanup_depth > 0 {
            self.error_at(
                "PropagationInCleanup",
                "a defer cleanup cannot propagate an error from its enclosing function",
                expression,
            );
        }

        let operand = self.check_expr(value, None, ExpressionContext::Value);
        let TyData::Result { ok, error } = self.types().data(operand).clone() else {
            if self.types().data(operand) != &TyData::Error {
                self.error_at(
                    "PropagationRequiresResult",
                    "the `?` operand must have a Result type",
                    expression,
                );
            }
            return self.types().error();
        };

        let return_ty = self.environment.return_ty;
        match self.types().data(return_ty).clone() {
            TyData::Result {
                error: return_error,
                ..
            } if return_error == error => {}
            TyData::Result { .. } => self.error_at(
                "PropagationErrorTypeMismatch",
                "the propagated error type must exactly match the current callable's Result error type",
                expression,
            ),
            TyData::Error => {}
            _ => self.error_at(
                "PropagationRequiresResultReturn",
                "a callable using `?` must return Result",
                expression,
            ),
        }
        ok
    }

    fn check_tuple(&mut self, elements: &[ExprId], expected: Option<TyId>) -> TyId {
        let arity = elements.len();
        let expected = expected.and_then(|expected| match self.types().data(expected).clone() {
            TyData::Tuple(elements) if elements.len() == arity => Some(elements),
            _ => None,
        });
        let elements = elements
            .iter()
            .enumerate()
            .map(|(index, element)| {
                self.check_expr(
                    *element,
                    expected
                        .as_ref()
                        .and_then(|expected| expected.get(index))
                        .copied(),
                    ExpressionContext::Value,
                )
            })
            .collect();
        self.types().intern(TyData::Tuple(elements))
    }

    fn check_list(
        &mut self,
        expression: ExprId,
        elements: &[ExprId],
        expected: Option<TyId>,
    ) -> TyId {
        let expected_element = expected.and_then(|expected| match self.types().data(expected) {
            TyData::List(element) => Some(*element),
            _ => None,
        });
        let Some(first) = elements.first().copied() else {
            let element = expected_element.unwrap_or_else(|| {
                self.error_at(
                    "CannotInferListElement",
                    "an empty list literal requires an expected List[T] type",
                    expression,
                );
                self.types().error()
            });
            return self.types().intern(TyData::List(element));
        };
        let element = self.check_expr(first, expected_element, ExpressionContext::Value);
        let element = expected_element.unwrap_or(element);
        for value in &elements[1..] {
            self.check_expr(*value, Some(element), ExpressionContext::Value);
        }
        self.types().intern(TyData::List(element))
    }

    fn check_literal(&mut self, expression: ExprId, literal: &Literal) -> TyId {
        match literal {
            Literal::Bool(_) => self.types().builtin(BuiltinType::Bool),
            Literal::Int(value) => {
                if value.parse::<i64>().is_err() {
                    self.error_at(
                        "IntegerLiteralOutOfRange",
                        "integer literal is outside the Int range",
                        expression,
                    );
                }
                self.types().builtin(BuiltinType::Int)
            }
            Literal::Float(value) => {
                if value.parse::<f64>().is_err() || value.parse::<f64>().is_ok_and(f64::is_infinite)
                {
                    self.error_at(
                        "FloatLiteralOutOfRange",
                        "float literal is outside the finite literal range",
                        expression,
                    );
                }
                self.types().builtin(BuiltinType::Float)
            }
            Literal::Text(_) => self.types().builtin(BuiltinType::Text),
            Literal::Unit => self.types().builtin(BuiltinType::Unit),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn check_path(&mut self, expression: ExprId, path: &Path, expected: Option<TyId>) -> TyId {
        if path.segments.len() == 1 {
            let name = &path.segments[0].name;
            for scope in self.scopes.iter().rev() {
                if let Some(local) = scope.get(name) {
                    let local = *local;
                    if self.scoped_locals.contains(&local)
                        && !self.checking_assignment_target
                        && !self.checking_scoped_receiver
                    {
                        self.error_at(
                            "ScopedValueCopy",
                            "a scoped resource cannot be copied, returned, stored, or passed as a value",
                            expression,
                        );
                    }
                    self.semantics
                        .expression_resolutions
                        .insert(expression, Resolution::Local(local));
                    let ty = self
                        .semantics
                        .local_types
                        .get(local)
                        .copied()
                        .unwrap_or_else(|| self.types().error());
                    if !self.checking_assignment_target {
                        self.task_local_uses.insert(local);
                    }
                    let mutable = self.source().locals[local].mutable;
                    if mutable && self.environment.contract != ContractMode::None {
                        self.error_at(
                            "InvalidContractExpression",
                            "contract predicates may only read immutable locals",
                            expression,
                        );
                    }
                    let place = Place {
                        root: PlaceRoot::Local(local),
                        projections: Vec::new(),
                        mutability: if mutable {
                            Mutability::Mutable
                        } else {
                            Mutability::ReadOnly
                        },
                    };
                    if !self.checking_assignment_target && !self.checking_view_source {
                        self.check_borrowed_place_use(&place, PlaceAccess::Read, expression);
                    }
                    self.semantics.expression_places.insert(expression, place);
                    return ty;
                }
            }
            if let Some((parameter, ty)) = self.environment.params.get(name).copied() {
                self.semantics
                    .expression_resolutions
                    .insert(expression, Resolution::Param(parameter));
                let place = Place {
                    root: PlaceRoot::Param(parameter),
                    projections: Vec::new(),
                    mutability: Mutability::ReadOnly,
                };
                if !self.checking_assignment_target && !self.checking_view_source {
                    self.check_borrowed_place_use(&place, PlaceAccess::Read, expression);
                }
                self.semantics.expression_places.insert(expression, place);
                return ty;
            }
            match name.as_str() {
                "Unit" => {
                    self.semantics
                        .expression_resolutions
                        .insert(expression, Resolution::Builtin(BuiltinValue::Unit));
                    return self.types().builtin(BuiltinType::Unit);
                }
                "None" => {
                    self.semantics
                        .expression_resolutions
                        .insert(expression, Resolution::Builtin(BuiltinValue::None));
                    if let Some(expected) = expected
                        && matches!(self.types().data(expected), TyData::Option(_))
                    {
                        return expected;
                    }
                    self.error_at(
                        "CannotInferType",
                        "`None` needs an expected Option type",
                        expression,
                    );
                    return self.types().error();
                }
                _ => {}
            }
        }
        let module = self.analyzer.program.definitions[self.environment.owner].module;
        if let Some(definition) = self
            .analyzer
            .resolve_definition(path, Namespace::Value, module)
        {
            self.semantics
                .expression_resolutions
                .insert(expression, Resolution::Definition(definition));
            self.error_at(
                "TypeMismatch",
                "functions are not first-class values; call this name",
                expression,
            );
        } else {
            self.semantics
                .expression_resolutions
                .insert(expression, Resolution::Error);
        }
        self.types().error()
    }

    fn check_self(&mut self, expression: ExprId) -> TyId {
        let Some(ty) = self.environment.self_ty else {
            self.error_at("UnknownName", "`self` is not available here", expression);
            return self.types().error();
        };
        if self.self_dirty && !self.allow_dirty_self_projection {
            self.error_at(
                "InvariantIsolationViolation",
                "a mutated receiver cannot escape or cross another call before its invariant boundary",
                expression,
            );
        }
        self.semantics
            .expression_resolutions
            .insert(expression, Resolution::SelfValue);
        let mutable = self.environment.receiver == Some(ReceiverKind::Mutable);
        let place = Place {
            root: PlaceRoot::SelfValue,
            projections: Vec::new(),
            mutability: if mutable {
                Mutability::Mutable
            } else {
                Mutability::ReadOnly
            },
        };
        if !self.checking_assignment_target && !self.checking_view_source {
            self.check_borrowed_place_use(&place, PlaceAccess::Read, expression);
        }
        self.semantics.expression_places.insert(expression, place);
        ty
    }

    fn check_result(&mut self, expression: ExprId) -> TyId {
        let Some(ty) = self.environment.result_ty else {
            self.error_at(
                "UnknownName",
                "`result` is only available in ensures",
                expression,
            );
            return self.types().error();
        };
        self.semantics
            .expression_resolutions
            .insert(expression, Resolution::ResultValue);
        ty
    }

    fn check_old(&mut self, expression: ExprId, value: ExprId) -> TyId {
        if self.environment.contract != (ContractMode::Predicate { old: true }) {
            self.error_at(
                "InvalidOldExpression",
                "old(expr) is only available in ensures",
                expression,
            );
        }
        let ty = self.check_expr(value, None, ExpressionContext::Value);
        if matches!(self.types().data(ty), TyData::View { .. }) {
            self.error_at(
                "OldOfView",
                "an erased interface parameter cannot be snapshotted",
                expression,
            );
        }
        ty
    }

    #[allow(clippy::too_many_lines)]
    fn check_block(
        &mut self,
        statements: &[Statement],
        tail: Option<ExprId>,
        expected: Option<TyId>,
        _context: ExpressionContext,
    ) -> TyId {
        self.scopes.push(BTreeMap::new());
        let region = RegionId(self.next_region);
        self.next_region = self.next_region.saturating_add(1);
        self.regions.push(region);
        let mut diverges = false;
        for statement in statements {
            match statement {
                Statement::Let { local, value } | Statement::Scoped { local, value } => {
                    let scoped = matches!(statement, Statement::Scoped { .. });
                    if scoped && self.cleanup_depth > 0 {
                        self.error(
                            "CleanupRegistrationInCleanup",
                            "a defer cleanup cannot register a scoped resource",
                            self.local_span(*local),
                        );
                    }
                    let annotation = self.source().locals[*local].annotation;
                    let expected = annotation.map(|annotation| {
                        let context = self.analyzer.type_context(self.environment.owner);
                        self.analyzer.resolve_type_ref(annotation, &context)
                    });
                    let ty =
                        self.check_suspendable_expr(*value, expected, ExpressionContext::Value);
                    diverges |= self.analyzer.typed.types.data(ty) == &TyData::Never;
                    self.semantics.local_types.insert(*local, ty);
                    if scoped {
                        self.check_scoped_binding(*local, *value, ty, region);
                    } else if self.has_marker_obligation(ty, MUST_SCOPE_CONCEPT) {
                        self.error(
                            "MustScopeRequiresScoped",
                            "this value has a MustScope obligation and must be bound with `scoped`",
                            self.local_span(*local),
                        );
                    }
                    let name = self.source().locals[*local].name.clone();
                    let scope = self.scopes.last_mut().expect("block scope exists");
                    if let Some(previous) = scope.insert(name.clone(), *local) {
                        self.analyzer.diagnostics.push(
                            Diagnostic::error(
                                "DuplicateDeclaration",
                                format!("local `{name}` is declared more than once"),
                                self.local_span(*local),
                            )
                            .with_label(self.local_span(previous), "first declaration"),
                        );
                    }
                }
                Statement::LetTuple { locals, value } => {
                    let ty = self.check_suspendable_expr(*value, None, ExpressionContext::Value);
                    diverges |= self.analyzer.typed.types.data(ty) == &TyData::Never;
                    let element_types = match self.types().data(ty).clone() {
                        TyData::Tuple(elements) if elements.len() == locals.len() => elements,
                        TyData::Tuple(elements) => {
                            self.error_at(
                                "TupleArityMismatch",
                                format!(
                                    "tuple binding declares {} names, but the value has {} elements",
                                    locals.len(),
                                    elements.len()
                                ),
                                *value,
                            );
                            vec![self.types().error(); locals.len()]
                        }
                        TyData::Error | TyData::Never => {
                            vec![self.types().error(); locals.len()]
                        }
                        _ => {
                            self.error_at(
                                "TupleBindingRequiresTuple",
                                "multiple local names require a tuple value",
                                *value,
                            );
                            vec![self.types().error(); locals.len()]
                        }
                    };
                    for (local, element_ty) in locals.iter().zip(element_types) {
                        self.semantics.local_types.insert(*local, element_ty);
                        if self.has_marker_obligation(element_ty, MUST_SCOPE_CONCEPT) {
                            self.error(
                                "MustScopeRequiresScoped",
                                "a tuple element with a MustScope obligation cannot be bound by ordinary `let`",
                                self.local_span(*local),
                            );
                        }
                        let name = self.source().locals[*local].name.clone();
                        let scope = self.scopes.last_mut().expect("block scope exists");
                        if let Some(previous) = scope.insert(name.clone(), *local) {
                            self.analyzer.diagnostics.push(
                                Diagnostic::error(
                                    "DuplicateDeclaration",
                                    format!("local `{name}` is declared more than once"),
                                    self.local_span(*local),
                                )
                                .with_label(self.local_span(previous), "first declaration"),
                            );
                        }
                    }
                }
                Statement::ForRange {
                    local,
                    start,
                    end,
                    body,
                } => {
                    let int = self.types().builtin(BuiltinType::Int);
                    let start_ty =
                        self.check_suspendable_expr(*start, Some(int), ExpressionContext::Value);
                    let end_ty =
                        self.check_suspendable_expr(*end, Some(int), ExpressionContext::Value);
                    self.semantics.expression_types.insert(*start, start_ty);
                    self.semantics.expression_types.insert(*end, end_ty);
                    self.semantics.local_types.insert(*local, int);

                    // The iteration binding belongs to the loop body's lexical
                    // environment. Keep an outer binding with the same name
                    // intact after checking the nested block.
                    let name = self.source().locals[*local].name.clone();
                    let previous = self
                        .scopes
                        .last_mut()
                        .expect("loop is inside a block scope")
                        .insert(name.clone(), *local);
                    let unit = self.types().builtin(BuiltinType::Unit);
                    self.check_expr(*body, Some(unit), ExpressionContext::Value);
                    let scope = self.scopes.last_mut().expect("loop block scope exists");
                    if let Some(previous) = previous {
                        scope.insert(name, previous);
                    } else {
                        scope.remove(&name);
                    }
                }
                Statement::Defer { body } => {
                    if self.cleanup_depth > 0 {
                        self.error_at(
                            "CleanupRegistrationInCleanup",
                            "a defer cleanup cannot register another cleanup",
                            *body,
                        );
                    }
                    self.cleanup_depth = self.cleanup_depth.saturating_add(1);
                    let unit = self.types().builtin(BuiltinType::Unit);
                    let ty = self.check_expr(*body, Some(unit), ExpressionContext::Value);
                    self.cleanup_depth = self.cleanup_depth.saturating_sub(1);
                    diverges |= self.analyzer.typed.types.data(ty) == &TyData::Never;
                }
                Statement::Expr(expression) => {
                    let unit = self.types().builtin(BuiltinType::Unit);
                    let ty = self.check_suspendable_expr(
                        *expression,
                        None,
                        ExpressionContext::UnitStatement,
                    );
                    if self.analyzer.typed.types.data(ty) == &TyData::Never {
                        diverges = true;
                    } else {
                        if self.has_marker_obligation(ty, MUST_SCOPE_CONCEPT) {
                            self.error_at(
                                "MustScopeRequiresScoped",
                                "discarding a MustScope value would lose its cleanup obligation",
                                *expression,
                            );
                        }
                        let coerced = self.coerce(*expression, ty, unit);
                        self.semantics.expression_types.insert(*expression, coerced);
                    }
                }
                Statement::Assert(predicate) => {
                    let bool_ty = self.types().builtin(BuiltinType::Bool);
                    let previous = self.environment.contract;
                    self.environment.contract = ContractMode::Predicate { old: false };
                    let ty = self.check_expr(*predicate, Some(bool_ty), ExpressionContext::Value);
                    diverges |= self.expression_diverges(*predicate)
                        || self.analyzer.typed.types.data(ty) == &TyData::Never;
                    self.environment.contract = previous;
                }
            }
        }
        let tail_result = if let Some(tail) = tail {
            self.check_suspendable_expr(tail, expected, ExpressionContext::Value)
        } else {
            self.types().builtin(BuiltinType::Unit)
        };
        let result = if diverges {
            self.types().never()
        } else {
            tail_result
        };
        self.borrows.retain(|borrow| borrow.region != region);
        self.active_no_suspend
            .retain(|(_, active_region, _)| *active_region != region);
        let locals = self
            .scopes
            .last()
            .into_iter()
            .flat_map(BTreeMap::values)
            .copied()
            .collect::<Vec<_>>();
        for local in locals {
            let is_task = self
                .semantics
                .local_types
                .get(local)
                .copied()
                .is_some_and(|ty| matches!(self.analyzer.typed.types.data(ty), TyData::Task(_)));
            if is_task && !self.task_local_uses.contains(&local) {
                self.error(
                    "UnawaitedAsyncCall",
                    "a stored Task must be awaited, joined, or returned before its lexical scope exits",
                    self.local_span(local),
                );
            }
        }
        self.regions.pop();
        self.scopes.pop();
        result
    }

    fn check_scoped_binding(
        &mut self,
        local: LocalId,
        expression: ExprId,
        ty: TyId,
        region: RegionId,
    ) {
        let intrinsic = match self.types().data(ty) {
            TyData::Builtin(BuiltinType::File) => Some(BuiltinValue::FileClose),
            TyData::Builtin(BuiltinType::Socket) => Some(BuiltinValue::SocketClose),
            _ => None,
        };
        if let Some(dispose) = intrinsic {
            self.semantics
                .scoped_disposals
                .insert(local, ScopedDisposal::Builtin(dispose));
            self.scoped_locals.insert(local);
            return;
        }
        let Some(dispose) = self.language_concept(DISPOSE_CONCEPT) else {
            self.error(
                "MissingDisposeConcept",
                "`scoped` requires the canonical standard.resource.Dispose concept",
                self.local_span(local),
            );
            return;
        };
        let Some(requirement) = self.concept_method(dispose, &Name::new("dispose")) else {
            self.error(
                "InvalidDisposeConcept",
                "standard.resource.Dispose must declare `method dispose(mut self) Unit`",
                self.local_span(local),
            );
            return;
        };
        let instance = ConceptInstance {
            concept: dispose,
            bindings: Vec::new(),
        };
        let Some(witness) = self.solve_resource_witness(ty, instance) else {
            self.error_at(
                "ScopedRequiresDispose",
                format!(
                    "{} does not conform to standard.resource.Dispose",
                    self.type_name(ty)
                ),
                expression,
            );
            return;
        };
        self.semantics.scoped_disposals.insert(
            local,
            ScopedDisposal::Concept {
                requirement,
                witness,
            },
        );
        self.scoped_locals.insert(local);
        if self
            .has_marker_conformance(ty, NO_SUSPEND_CONCEPT)
            .is_some()
        {
            self.active_no_suspend
                .push((local, region, self.local_span(local)));
        }
    }

    fn check_if(
        &mut self,
        condition: ExprId,
        then_branch: ExprId,
        else_branch: Option<ExprId>,
        expected: Option<TyId>,
        context: ExpressionContext,
    ) -> TyId {
        let bool_ty = self.types().builtin(BuiltinType::Bool);
        self.check_expr(condition, Some(bool_ty), ExpressionContext::Value);
        let entry = self.flow_state();
        let then_ty = self.check_expr(then_branch, expected, context);
        let then_diverges = self.expression_diverges(then_branch);
        let then_state = self.flow_state();
        if let Some(else_branch) = else_branch {
            self.restore_flow(&entry);
            let expected_else = expected.or_else(|| (!then_diverges).then_some(then_ty));
            let else_ty = self.check_expr(else_branch, expected_else, context);
            let else_diverges = self.expression_diverges(else_branch);
            let else_state = self.flow_state();
            match (then_diverges, else_diverges) {
                (true, true) => {
                    self.restore_flow(&entry);
                    self.types().never()
                }
                (true, false) => {
                    self.restore_flow(&else_state);
                    else_ty
                }
                (false, true) => {
                    self.restore_flow(&then_state);
                    then_ty
                }
                (false, false) => {
                    self.join_flow_states([then_state, else_state]);
                    self.join_types(then_ty, else_ty, self.expr_span(else_branch))
                }
            }
        } else {
            if then_diverges {
                self.restore_flow(&entry);
            } else {
                self.join_flow_states([entry, then_state]);
            }
            let unit = self.types().builtin(BuiltinType::Unit);
            if context != ExpressionContext::UnitStatement && expected != Some(unit) {
                self.error_at("MissingElse", "if expression requires else", then_branch);
            }
            self.coerce(then_branch, then_ty, unit);
            unit
        }
    }

    fn check_unary(&mut self, expression: ExprId, operator: UnaryOp, operand: ExprId) -> TyId {
        // The positive magnitude of Int.MIN is one past i64::MAX. It is legal
        // only as the immediate operand of unary `-`; keep that one token out
        // of the ordinary literal range diagnostic so lowering can materialize
        // i64::MIN without relying on overflow or target behavior.
        let is_int_min = operator == UnaryOp::Negate
            && matches!(
                &self.source().expressions[operand],
                Expr::Literal(Literal::Int(value)) if value == "9223372036854775808"
            );
        let operand_ty = if is_int_min {
            let int = self.types().builtin(BuiltinType::Int);
            self.semantics.expression_types.insert(operand, int);
            int
        } else {
            self.check_expr(operand, None, ExpressionContext::Value)
        };
        match operator {
            UnaryOp::Not => {
                let bool_ty = self.types().builtin(BuiltinType::Bool);
                self.coerce(operand, operand_ty, bool_ty);
                bool_ty
            }
            UnaryOp::Negate => {
                if self.is_builtin_or_refined(operand_ty, BuiltinType::Int) {
                    self.types().builtin(BuiltinType::Int)
                } else if self.is_builtin_or_refined(operand_ty, BuiltinType::Float) {
                    self.types().builtin(BuiltinType::Float)
                } else {
                    self.error_at(
                        "InvalidGenericOperation",
                        "unary `-` requires Int or Float",
                        expression,
                    );
                    self.types().error()
                }
            }
        }
    }

    fn check_binary(
        &mut self,
        expression: ExprId,
        operator: BinaryOp,
        left: ExprId,
        right: ExprId,
    ) -> TyId {
        let left_ty = self.check_expr(left, None, ExpressionContext::Value);
        let right_ty = self.check_expr(right, None, ExpressionContext::Value);
        let bool_ty = self.types().builtin(BuiltinType::Bool);
        match operator {
            BinaryOp::And | BinaryOp::Or => {
                self.coerce(left, left_ty, bool_ty);
                self.coerce(right, right_ty, bool_ty);
                bool_ty
            }
            BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide => {
                let numeric = if self.is_builtin_or_refined(left_ty, BuiltinType::Int)
                    && self.is_builtin_or_refined(right_ty, BuiltinType::Int)
                {
                    Some(BuiltinType::Int)
                } else if self.is_builtin_or_refined(left_ty, BuiltinType::Float)
                    && self.is_builtin_or_refined(right_ty, BuiltinType::Float)
                {
                    Some(BuiltinType::Float)
                } else {
                    None
                };
                if let Some(numeric) = numeric {
                    if self.environment.contract != ContractMode::None
                        && numeric == BuiltinType::Int
                    {
                        self.error_at(
                            "InvalidContractExpression",
                            "Int arithmetic is not total and cannot appear in a contract",
                            expression,
                        );
                    }
                    self.types().builtin(numeric)
                } else {
                    self.error_at(
                        "InvalidGenericOperation",
                        "arithmetic operands must have the same numeric base type",
                        expression,
                    );
                    self.types().error()
                }
            }
            BinaryOp::Less | BinaryOp::LessEqual | BinaryOp::Greater | BinaryOp::GreaterEqual => {
                let comparable = (self.is_builtin_or_refined(left_ty, BuiltinType::Int)
                    && self.is_builtin_or_refined(right_ty, BuiltinType::Int))
                    || (self.is_builtin_or_refined(left_ty, BuiltinType::Float)
                        && self.is_builtin_or_refined(right_ty, BuiltinType::Float));
                if !comparable {
                    self.error_at(
                        "InvalidGenericOperation",
                        "ordered comparison requires matching Int or Float operands",
                        expression,
                    );
                }
                bool_ty
            }
            BinaryOp::Equal | BinaryOp::NotEqual => {
                if !self.assignable(left_ty, right_ty) && !self.assignable(right_ty, left_ty) {
                    self.error_at(
                        "TypeMismatch",
                        "equality operands have incompatible types",
                        expression,
                    );
                }
                bool_ty
            }
        }
    }

    fn check_assignment(&mut self, expression: ExprId, target: ExprId, value: ExprId) -> TyId {
        self.checking_assignment_target = true;
        let target_ty = self.check_expr(target, None, ExpressionContext::Value);
        self.checking_assignment_target = false;
        let Some(place) = self.semantics.expression_places.get(target).cloned() else {
            self.error_at(
                "InvalidAssignmentTarget",
                "assignment target is not a writable place",
                target,
            );
            self.check_expr(value, Some(target_ty), ExpressionContext::Value);
            return self.types().builtin(BuiltinType::Unit);
        };
        if place.mutability != Mutability::Mutable {
            self.error_at(
                "ImmutableBindingAssignment",
                "assignment target is immutable",
                target,
            );
        }
        self.check_borrowed_place_use(&place, PlaceAccess::Write, target);
        if !place.projections.is_empty() && !matches!(place.root, PlaceRoot::SelfValue) {
            self.error_at(
                "ReadonlyReceiverMutation",
                "record fields can only be changed through the owning type's mut self method",
                target,
            );
        }
        self.check_expr(value, Some(target_ty), ExpressionContext::Value);
        if matches!(place.root, PlaceRoot::SelfValue) && !place.projections.is_empty() {
            self.self_dirty = true;
        }
        let unit = self.types().builtin(BuiltinType::Unit);
        self.semantics.expression_types.insert(expression, unit);
        unit
    }

    fn check_return(&mut self, value: Option<ExprId>) -> TyId {
        if self.cleanup_depth > 0 {
            self.error(
                "ReturnFromCleanup",
                "a defer cleanup cannot return from its enclosing function",
                self.body_span(),
            );
        }
        let return_ty = self.environment.return_ty;
        if let Some(value) = value {
            self.check_suspendable_expr(value, Some(return_ty), ExpressionContext::Value);
        } else {
            let unit = self.types().builtin(BuiltinType::Unit);
            if return_ty != unit {
                self.error(
                    "TypeMismatch",
                    "bare return is only valid for a Unit-returning callable",
                    self.body_span(),
                );
            }
        }
        self.types().never()
    }

    fn check_field(&mut self, expression: ExprId, receiver: ExprId, name: &Name) -> TyId {
        if let Expr::Path(type_path) = self.source().expressions[receiver].clone()
            && let Some((variant, owner)) = self.resolve_qualified_variant(&type_path, name)
        {
            return self.check_variant_constructor(expression, variant, owner, &[], None);
        }
        let previous = self.allow_dirty_self_projection;
        let previous_scoped = self.checking_scoped_receiver;
        self.allow_dirty_self_projection = true;
        self.checking_scoped_receiver = true;
        let receiver_ty = self.check_expr(receiver, None, ExpressionContext::Value);
        self.allow_dirty_self_projection = previous;
        self.checking_scoped_receiver = previous_scoped;
        let Some((definition, arguments)) = self.nominal_parts(receiver_ty) else {
            self.error_at(
                "UnknownField",
                format!("type has no field `{name}`"),
                expression,
            );
            return self.types().error();
        };
        let DefinitionKind::Record(record) = &self.analyzer.program.definitions[definition].kind
        else {
            self.error_at(
                "UnknownField",
                format!("type has no field `{name}`"),
                expression,
            );
            return self.types().error();
        };
        let Some(field) =
            record.fields.iter().copied().find(|field| {
                self.analyzer.program.definitions[*field].name.as_ref() == Some(name)
            })
        else {
            self.error_at(
                "UnknownField",
                format!("record has no field `{name}`"),
                expression,
            );
            return self.types().error();
        };
        let Some(Signature::Field { ty, .. }) = self.analyzer.typed.signatures.get(field) else {
            return self.types().error();
        };
        let ty = *ty;
        let substitution =
            substitution_for(&self.analyzer.type_generic_params(definition), &arguments);
        let ty = self.types().substitute(ty, &substitution);
        if let Some(mut place) = self.semantics.expression_places.get(receiver).cloned() {
            place.projections.push(PlaceProjection::Field(field));
            self.semantics.expression_places.insert(expression, place);
        }
        ty
    }

    fn check_match(
        &mut self,
        scrutinee: ExprId,
        arms: &[MatchArm],
        expected: Option<TyId>,
        context: ExpressionContext,
    ) -> TyId {
        let scrutinee_ty = self.check_expr(scrutinee, None, ExpressionContext::Value);
        let mut result = expected;
        let mut coverage = PatternCoverage::default();
        let entry = self.flow_state();
        let mut exits = Vec::new();
        let mut has_reachable_arm = false;
        for arm in arms {
            self.restore_flow(&entry);
            self.scopes.push(BTreeMap::new());
            self.check_pattern(arm.pattern, scrutinee_ty, &mut coverage);
            let arm_ty = self.check_expr(arm.value, result, context);
            self.scopes.pop();
            if !self.expression_diverges(arm.value) {
                has_reachable_arm = true;
                result = Some(match result {
                    Some(previous) => self.join_types(previous, arm_ty, arm.span),
                    None => arm_ty,
                });
                exits.push(self.flow_state());
            }
        }
        if exits.is_empty() {
            self.restore_flow(&entry);
        } else {
            self.join_flow_states(exits);
        }
        self.check_exhaustive(scrutinee_ty, &coverage, scrutinee);
        if !has_reachable_arm && !arms.is_empty() {
            self.types().never()
        } else {
            result.unwrap_or_else(|| self.types().error())
        }
    }

    fn check_pattern(
        &mut self,
        pattern: PatternId,
        expected: TyId,
        coverage: &mut PatternCoverage,
    ) {
        self.semantics.pattern_types.insert(pattern, expected);
        match self.source().patterns[pattern].clone() {
            Pattern::Error => {
                self.semantics
                    .pattern_resolutions
                    .insert(pattern, Resolution::Error);
            }
            Pattern::Wildcard => coverage.catch_all = true,
            Pattern::Binding(local) => self.bind_pattern(pattern, local, expected, coverage),
            Pattern::Literal(literal) => {
                let literal_ty = match literal {
                    Literal::Bool(_) => self.types().builtin(BuiltinType::Bool),
                    Literal::Int(_) => self.types().builtin(BuiltinType::Int),
                    Literal::Float(_) => self.types().builtin(BuiltinType::Float),
                    Literal::Text(_) => self.types().builtin(BuiltinType::Text),
                    Literal::Unit => self.types().builtin(BuiltinType::Unit),
                };
                self.expect_compatible(pattern, literal_ty, expected);
                if let Literal::Bool(value) = literal {
                    coverage.bool_values.insert(value);
                }
            }
            Pattern::Name {
                path,
                payload,
                binding,
            } => {
                if let Some(variant) = self.resolve_pattern_variant(&path, &payload, expected) {
                    self.semantics
                        .pattern_resolutions
                        .insert(pattern, pattern_variant_resolution(variant));
                    coverage.variants.insert(variant);
                    let payload_types = self.variant_payload(variant, expected);
                    if payload_types.len() != payload.len() {
                        self.error(
                            "TypeMismatch",
                            "variant pattern payload has the wrong arity",
                            self.pattern_span(pattern),
                        );
                    }
                    for (child, ty) in payload.iter().zip(payload_types) {
                        self.check_pattern(*child, ty, &mut PatternCoverage::default());
                    }
                } else if let Some(binding) = binding {
                    self.bind_pattern(pattern, binding, expected, coverage);
                } else {
                    self.error(
                        "UnknownName",
                        format!("unknown variant `{}`", path.as_string()),
                        self.pattern_span(pattern),
                    );
                    self.semantics
                        .pattern_resolutions
                        .insert(pattern, Resolution::Error);
                }
            }
            Pattern::Variant { path, payload } => {
                if let Some(variant) = self.resolve_pattern_variant(&path, &payload, expected) {
                    self.semantics
                        .pattern_resolutions
                        .insert(pattern, pattern_variant_resolution(variant));
                }
            }
        }
    }

    fn bind_pattern(
        &mut self,
        pattern: PatternId,
        local: LocalId,
        expected: TyId,
        coverage: &mut PatternCoverage,
    ) {
        coverage.catch_all = true;
        self.semantics
            .pattern_resolutions
            .insert(pattern, Resolution::Local(local));
        self.semantics.local_types.insert(local, expected);
        let name = self.source().locals[local].name.clone();
        self.scopes
            .last_mut()
            .expect("pattern scope exists")
            .insert(name, local);
    }

    fn coerce(&mut self, expression: ExprId, actual: TyId, expected: TyId) -> TyId {
        if actual == expected {
            if self.dynamic_coercion_mode == DynamicCoercionMode::CallBorrow
                && let TyData::View { mutability, .. } =
                    self.analyzer.typed.types.data(expected).clone()
            {
                self.reborrow_interface_argument(expression, mutability);
            }
            return expected;
        }
        if self.is_error(actual) || self.is_error(expected) {
            return expected;
        }
        if self.types().data(actual) == &TyData::Never {
            self.semantics
                .expression_coercions
                .insert(expression, Coercion::NeverToAny);
            return expected;
        }
        let dynamic_expected = match self.types().data(expected).clone() {
            TyData::View { mutability, target }
                if !matches!(self.types().data(actual), TyData::View { .. }) =>
            {
                Some((mutability, target))
            }
            _ => None,
        };
        if let Some((mutability, target)) = dynamic_expected {
            return self.coerce_to_dynamic(expression, actual, expected, mutability, target);
        }
        if let TyData::Nominal { definition, .. } = self.types().data(actual).clone()
            && let DefinitionKind::RefinedType(refined) =
                &self.analyzer.program.definitions[definition].kind
        {
            let base = self
                .analyzer
                .typed
                .resolved_type_refs
                .get(refined.base)
                .copied()
                .unwrap_or_else(|| self.types().error());
            if base == expected {
                self.semantics.expression_coercions.insert(
                    expression,
                    Coercion::RefinedToBase {
                        refined: definition,
                    },
                );
                return expected;
            }
        }
        self.error_at(
            "TypeMismatch",
            format!(
                "expected {}, found {}",
                self.type_name(expected),
                self.type_name(actual)
            ),
            expression,
        );
        self.types().error()
    }

    fn coerce_to_dynamic(
        &mut self,
        expression: ExprId,
        actual: TyId,
        expected: TyId,
        mutability: Mutability,
        target: TyId,
    ) -> TyId {
        let TyData::DynTarget(instance) = self.types().data(target).clone() else {
            self.error_at(
                "CompilerDefect",
                "dynamic parameter has no concept target",
                expression,
            );
            return self.types().error();
        };
        let owner = self.semantics.expression_places.get(expression).cloned();
        let borrowed =
            self.dynamic_coercion_mode == DynamicCoercionMode::CallBorrow && owner.is_some();
        if self.dynamic_coercion_mode == DynamicCoercionMode::CallBorrow
            && mutability == Mutability::Mutable
            && owner
                .as_ref()
                .is_none_or(|owner| owner.mutability != Mutability::Mutable)
        {
            self.error_at(
                "DynMutReceiverUnavailable",
                "this concept can modify its receiver, so the argument must be a variable",
                expression,
            );
        }
        if let Some(witness) = self.solve_witness(actual, instance, expression) {
            let region = *self.regions.last().expect("body has a lexical region");
            let token = ViewTokenId(self.next_view_token);
            self.next_view_token = self.next_view_token.saturating_add(1);
            if borrowed {
                self.register_borrow(
                    owner.clone().expect("borrowed interface has an owner"),
                    mutability == Mutability::Mutable,
                    region,
                    token,
                    self.expr_span(expression),
                    expression,
                );
            }
            self.semantics.views.insert(
                expression,
                ViewResolution {
                    source: ViewSource::Concrete {
                        witness,
                        writeback: borrowed.then(|| {
                            owner.expect("borrowed interface has an owner after registration")
                        }),
                    },
                    mutable: mutability == Mutability::Mutable,
                    region,
                    token,
                },
            );
            self.semantics
                .expression_coercions
                .insert(expression, Coercion::ConcreteToDyn);
        }
        expected
    }

    fn reborrow_interface_argument(&mut self, expression: ExprId, mutability: Mutability) {
        if self.semantics.views.get(expression).is_some() {
            return;
        }
        let Some(owner) = self.semantics.expression_places.get(expression).cloned() else {
            if mutability == Mutability::Mutable {
                self.error_at(
                    "DynMutReceiverUnavailable",
                    "a mutable interface argument must name a variable",
                    expression,
                );
            }
            return;
        };
        let mutable_owner =
            owner.mutability == Mutability::Mutable || matches!(owner.root, PlaceRoot::Param(_));
        if mutability == Mutability::Mutable && !mutable_owner {
            self.error_at(
                "DynMutReceiverUnavailable",
                "this interface can modify its receiver, so the argument must be a variable",
                expression,
            );
        }
        let region = *self.regions.last().expect("body has a lexical region");
        let token = ViewTokenId(self.next_view_token);
        self.next_view_token = self.next_view_token.saturating_add(1);
        self.register_borrow(
            owner.clone(),
            mutability == Mutability::Mutable,
            region,
            token,
            self.expr_span(expression),
            expression,
        );
        self.semantics.views.insert(
            expression,
            ViewResolution {
                source: ViewSource::Interface { owner },
                mutable: mutability == Mutability::Mutable,
                region,
                token,
            },
        );
        self.semantics
            .expression_coercions
            .insert(expression, Coercion::InterfaceReborrow);
    }

    fn assignable(&self, actual: TyId, expected: TyId) -> bool {
        if actual == expected || self.is_error(actual) || self.is_error(expected) {
            return true;
        }
        if self.analyzer.typed.types.data(actual) == &TyData::Never {
            return true;
        }
        if let TyData::Nominal { definition, .. } = self.analyzer.typed.types.data(actual)
            && let DefinitionKind::RefinedType(refined) =
                &self.analyzer.program.definitions[*definition].kind
        {
            return self
                .analyzer
                .typed
                .resolved_type_refs
                .get(refined.base)
                .is_some_and(|base| *base == expected);
        }
        false
    }

    fn is_builtin_or_refined(&self, ty: TyId, builtin: BuiltinType) -> bool {
        if self.analyzer.typed.types.data(ty) == &TyData::Builtin(builtin) {
            return true;
        }
        let TyData::Nominal { definition, .. } = self.analyzer.typed.types.data(ty) else {
            return false;
        };
        let DefinitionKind::RefinedType(refined) =
            &self.analyzer.program.definitions[*definition].kind
        else {
            return false;
        };
        self.analyzer
            .typed
            .resolved_type_refs
            .get(refined.base)
            .is_some_and(|base| self.analyzer.typed.types.data(*base) == &TyData::Builtin(builtin))
    }

    fn join_types(&mut self, left: TyId, right: TyId, span: Span) -> TyId {
        if self.assignable(right, left) {
            left
        } else if self.assignable(left, right) {
            right
        } else {
            self.error(
                "TypeMismatch",
                format!(
                    "branches have incompatible types {} and {}",
                    self.type_name(left),
                    self.type_name(right)
                ),
                span,
            );
            self.types().error()
        }
    }

    fn nominal_parts(&self, ty: TyId) -> Option<(DefId, Vec<TyId>)> {
        match self.analyzer.typed.types.data(ty) {
            TyData::Nominal {
                definition,
                arguments,
            } => Some((*definition, arguments.clone())),
            _ => None,
        }
    }

    fn is_error(&self, ty: TyId) -> bool {
        self.analyzer.typed.types.data(ty) == &TyData::Error
    }

    fn type_name(&self, ty: TyId) -> String {
        match self.analyzer.typed.types.data(ty) {
            TyData::Error => "<error>".to_owned(),
            TyData::Never => "Never".to_owned(),
            TyData::Builtin(builtin) => format!("{builtin:?}"),
            TyData::Tuple(elements) => format!(
                "({})",
                elements
                    .iter()
                    .map(|element| self.type_name(*element))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            TyData::List(element) => format!("List[{}]", self.type_name(*element)),
            TyData::Option(element) => format!("Option[{}]", self.type_name(*element)),
            TyData::Result { ok, error } => {
                format!(
                    "Result[{}, {}]",
                    self.type_name(*ok),
                    self.type_name(*error)
                )
            }
            TyData::Task(output) => format!("Task[{}]", self.type_name(*output)),
            TyData::TaskOutcome(output) => {
                format!("TaskOutcome[{}]", self.type_name(*output))
            }
            TyData::Nominal {
                definition,
                arguments,
            } => {
                let name = self.analyzer.program.definitions[*definition]
                    .name
                    .as_ref()
                    .map_or("<anonymous>", Name::as_str);
                if arguments.is_empty() {
                    name.to_owned()
                } else {
                    format!(
                        "{name}[{}]",
                        arguments
                            .iter()
                            .map(|argument| self.type_name(*argument))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            }
            TyData::Param(parameter) => self.analyzer.program.generic_params[*parameter]
                .name
                .to_string(),
            TyData::SelfType(_) => "Self".to_owned(),
            TyData::Projection { .. } => "<associated type>".to_owned(),
            TyData::DynTarget(_) => "dyn concept".to_owned(),
            TyData::View { mutability, .. } => format!("dyn concept ({mutability:?} receiver)"),
        }
    }

    fn source(&self) -> &loom_hir::Body {
        &self.analyzer.program.bodies[self.body]
    }

    fn types(&mut self) -> &mut crate::TyInterner {
        &mut self.analyzer.typed.types
    }

    fn expr_span(&self, expression: ExprId) -> Span {
        self.source()
            .source_map
            .expr(expression)
            .unwrap_or_default()
    }

    fn pattern_span(&self, pattern: PatternId) -> Span {
        self.source()
            .source_map
            .pattern(pattern)
            .unwrap_or_default()
    }

    fn local_span(&self, local: LocalId) -> Span {
        self.source().source_map.local(local).unwrap_or_default()
    }

    fn body_span(&self) -> Span {
        self.analyzer
            .program
            .source_map
            .body(self.body)
            .unwrap_or_default()
    }

    fn error_at(&mut self, code: &'static str, message: impl Into<String>, expression: ExprId) {
        self.error(code, message, self.expr_span(expression));
    }

    fn error(&mut self, code: &'static str, message: impl Into<String>, span: Span) {
        self.analyzer.error(code, message, span);
    }

    #[allow(clippy::too_many_lines)]
    fn check_call(
        &mut self,
        expression: ExprId,
        callee: ExprId,
        type_arguments: &[TypeRefId],
        arguments: &[ExprId],
        expected: Option<TyId>,
    ) -> TyId {
        let Expr::Path(path) = self.source().expressions[callee].clone() else {
            self.check_expr(callee, None, ExpressionContext::Value);
            self.error_at(
                "TypeMismatch",
                "only named functions and constructors are callable",
                expression,
            );
            return self.types().error();
        };
        if path.segments.len() == 1 && path.segments[0].name.as_str() == "List" {
            if !arguments.is_empty() {
                self.call_arity(expression, 0, arguments.len());
            }
            let explicit = self.resolve_call_type_arguments(type_arguments);
            let element = if explicit.len() == 1 {
                explicit[0]
            } else {
                self.error_at(
                    "TypeMismatch",
                    "List construction requires exactly one element type: `List[T]()`",
                    expression,
                );
                self.types().error()
            };
            let result = self.types().intern(TyData::List(element));
            self.semantics.calls.insert(
                expression,
                CallResolution {
                    target: CallTarget::Builtin(BuiltinValue::ListNew),
                    substitution: Substitution::default(),
                    dispatch_witness: None,
                    witnesses: Vec::new(),
                    receiver: None,
                },
            );
            self.finish_call_arguments(arguments);
            return result;
        }
        if path.segments.len() == 1 {
            let builtin = match path.segments[0].name.as_str() {
                "Some" => Some(BuiltinValue::Some),
                "Ok" => Some(BuiltinValue::Ok),
                "Err" => Some(BuiltinValue::Err),
                "parse_float" if self.builtin_is_imported("standard.float.parse_float") => {
                    Some(BuiltinValue::ParseFloat)
                }
                "format_float" if self.builtin_is_imported("standard.float.format_float") => {
                    Some(BuiltinValue::FormatFloat)
                }
                "is_finite" if self.builtin_is_imported("standard.float.is_finite") => {
                    Some(BuiltinValue::IsFinite)
                }
                "arguments" if self.builtin_is_imported("standard.process.arguments") => {
                    Some(BuiltinValue::ProcessArguments)
                }
                "environment" if self.builtin_is_imported("standard.process.environment") => {
                    Some(BuiltinValue::ProcessEnvironment)
                }
                "parse_int" if self.builtin_is_imported("standard.int.parse_int") => {
                    Some(BuiltinValue::ParseInt)
                }
                "milliseconds" if self.builtin_is_imported("standard.time.milliseconds") => {
                    Some(BuiltinValue::DurationMilliseconds)
                }
                "open_read" if self.builtin_is_imported("standard.file.open_read") => {
                    Some(BuiltinValue::FileOpenRead)
                }
                "create" if self.builtin_is_imported("standard.file.create") => {
                    Some(BuiltinValue::FileCreate)
                }
                "connect" if self.builtin_is_imported("standard.net.connect") => {
                    Some(BuiltinValue::SocketConnect)
                }
                _ => None,
            };
            if let Some(builtin) = builtin {
                return self.check_builtin_call(expression, builtin, arguments, expected);
            }
        }

        let module = self.analyzer.program.definitions[self.environment.owner].module;
        let resolver = crate::Resolver::new(self.analyzer.program, self.analyzer.def_maps, module);
        if let Ok(definition) = resolver.resolve_definition(&path, Namespace::Value) {
            let Some(Signature::Callable(signature)) =
                self.analyzer.typed.signatures.get(definition).cloned()
            else {
                self.error_at("TypeMismatch", "name is not callable", expression);
                return self.types().error();
            };
            let explicit = self.resolve_call_type_arguments(type_arguments);
            let (return_ty, substitution) = self.check_callable_arguments(
                expression,
                &signature,
                arguments,
                &explicit,
                expected,
                Substitution::default(),
            );
            self.finish_call_arguments(arguments);
            let witnesses = self.resolve_bound_witnesses(&signature, &substitution, expression);
            let return_ty =
                self.normalize_call_type(return_ty, &signature, &substitution, &witnesses);
            self.semantics.calls.insert(
                expression,
                CallResolution {
                    target: CallTarget::Function(definition),
                    substitution,
                    dispatch_witness: None,
                    witnesses,
                    receiver: None,
                },
            );
            return self.finish_async_call(expression, signature.is_async, return_ty);
        }
        if let Ok(definition) = resolver.resolve_definition(&path, Namespace::Type)
            && let DefinitionKind::RefinedType(refined) =
                &self.analyzer.program.definitions[definition].kind
        {
            if !type_arguments.is_empty() || arguments.len() != 1 {
                self.error_at(
                    "TypeMismatch",
                    "a constrained constructor takes exactly one value",
                    expression,
                );
                return self.types().error();
            }
            let base = self
                .analyzer
                .typed
                .resolved_type_refs
                .get(refined.base)
                .copied()
                .unwrap_or_else(|| self.types().error());
            self.check_expr(arguments[0], Some(base), ExpressionContext::Value);
            let nominal = self.types().intern(TyData::Nominal {
                definition,
                arguments: Vec::new(),
            });
            let violation = self.types().builtin(BuiltinType::Violation);
            let result = self.types().intern(TyData::Result {
                ok: nominal,
                error: violation,
            });
            self.semantics.calls.insert(
                expression,
                CallResolution {
                    target: CallTarget::RefinedConstructor(definition),
                    substitution: Substitution::default(),
                    dispatch_witness: None,
                    witnesses: Vec::new(),
                    receiver: None,
                },
            );
            return result;
        }
        self.error_at(
            "UnknownName",
            format!("unknown callable `{}`", path.as_string()),
            expression,
        );
        self.types().error()
    }

    fn finish_async_call(&mut self, _expression: ExprId, is_async: bool, output: TyId) -> TyId {
        if !is_async {
            return output;
        }
        self.types().intern(TyData::Task(output))
    }

    fn check_builtin_call(
        &mut self,
        expression: ExprId,
        builtin: BuiltinValue,
        arguments: &[ExprId],
        expected: Option<TyId>,
    ) -> TyId {
        let result = match builtin {
            BuiltinValue::Some => self.check_some_call(expression, arguments, expected),
            BuiltinValue::Ok | BuiltinValue::Err => {
                self.check_result_constructor_call(expression, builtin, arguments, expected)
            }
            BuiltinValue::ParseFloat
            | BuiltinValue::FormatFloat
            | BuiltinValue::IsFinite
            | BuiltinValue::ProcessArguments
            | BuiltinValue::ProcessEnvironment
            | BuiltinValue::ParseInt
            | BuiltinValue::DurationMilliseconds
            | BuiltinValue::FileOpenRead
            | BuiltinValue::FileCreate
            | BuiltinValue::SocketConnect => {
                self.check_standard_builtin_call(expression, builtin, arguments)
            }
            BuiltinValue::ListNew
            | BuiltinValue::ListAdd
            | BuiltinValue::ListLength
            | BuiltinValue::ListGet
            | BuiltinValue::TaskFaultCode
            | BuiltinValue::TaskFaultMessage
            | BuiltinValue::DurationAsMilliseconds
            | BuiltinValue::FileReadText
            | BuiltinValue::FileWriteText
            | BuiltinValue::FileClose
            | BuiltinValue::SocketReadText
            | BuiltinValue::SocketWriteText
            | BuiltinValue::SocketClose => {
                self.error_at(
                    "TypeMismatch",
                    "List builtin is only available through List construction or methods",
                    expression,
                );
                self.types().error()
            }
            BuiltinValue::Unit
            | BuiltinValue::None
            | BuiltinValue::ParseFloatInvalidSyntax
            | BuiltinValue::ParseFloatOutOfRange
            | BuiltinValue::ParseIntInvalidSyntax
            | BuiltinValue::ParseIntOutOfRange
            | BuiltinValue::TaskCompleted
            | BuiltinValue::TaskFaulted
            | BuiltinValue::TaskCancelled => {
                self.error_at(
                    "TypeMismatch",
                    "value constructor is not callable",
                    expression,
                );
                self.types().error()
            }
        };
        self.semantics.calls.insert(
            expression,
            CallResolution {
                target: CallTarget::Builtin(builtin),
                substitution: Substitution::default(),
                dispatch_witness: None,
                witnesses: Vec::new(),
                receiver: None,
            },
        );
        self.finish_call_arguments(arguments);
        result
    }

    fn check_some_call(
        &mut self,
        expression: ExprId,
        arguments: &[ExprId],
        expected: Option<TyId>,
    ) -> TyId {
        if arguments.len() != 1 {
            self.call_arity(expression, 1, arguments.len());
            return self.types().error();
        }
        let element_expected = expected.and_then(|expected| match self.types().data(expected) {
            TyData::Option(element) => Some(*element),
            _ => None,
        });
        let element = self.check_expr(arguments[0], element_expected, ExpressionContext::Value);
        self.types().intern(TyData::Option(element))
    }

    fn check_result_constructor_call(
        &mut self,
        expression: ExprId,
        builtin: BuiltinValue,
        arguments: &[ExprId],
        expected: Option<TyId>,
    ) -> TyId {
        if arguments.len() != 1 {
            self.call_arity(expression, 1, arguments.len());
            return self.types().error();
        }
        let Some(expected) = expected else {
            self.error_at(
                "CannotInferType",
                "Ok/Err require an expected Result type",
                expression,
            );
            return self.types().error();
        };
        let TyData::Result { ok, error } = self.types().data(expected).clone() else {
            self.error_at(
                "TypeMismatch",
                "Ok/Err require an expected Result type",
                expression,
            );
            return self.types().error();
        };
        let payload = if builtin == BuiltinValue::Ok {
            ok
        } else {
            error
        };
        self.check_expr(arguments[0], Some(payload), ExpressionContext::Value);
        expected
    }

    fn check_standard_builtin_call(
        &mut self,
        expression: ExprId,
        builtin: BuiltinValue,
        arguments: &[ExprId],
    ) -> TyId {
        match builtin {
            BuiltinValue::ParseFloat => {
                let text = self.types().builtin(BuiltinType::Text);
                self.check_fixed_arguments(expression, arguments, &[text]);
                let float = self.types().builtin(BuiltinType::Float);
                let error = self.types().builtin(BuiltinType::ParseFloatError);
                self.types().intern(TyData::Result { ok: float, error })
            }
            BuiltinValue::FormatFloat => {
                let float = self.types().builtin(BuiltinType::Float);
                self.check_fixed_arguments(expression, arguments, &[float]);
                self.types().builtin(BuiltinType::Text)
            }
            BuiltinValue::IsFinite => {
                let float = self.types().builtin(BuiltinType::Float);
                self.check_fixed_arguments(expression, arguments, &[float]);
                self.types().builtin(BuiltinType::Bool)
            }
            BuiltinValue::ProcessArguments => {
                self.check_fixed_arguments(expression, arguments, &[]);
                let text = self.types().builtin(BuiltinType::Text);
                self.types().intern(TyData::List(text))
            }
            BuiltinValue::ProcessEnvironment => {
                let text = self.types().builtin(BuiltinType::Text);
                self.check_fixed_arguments(expression, arguments, &[text]);
                self.types().intern(TyData::Option(text))
            }
            BuiltinValue::ParseInt => {
                let text = self.types().builtin(BuiltinType::Text);
                self.check_fixed_arguments(expression, arguments, &[text]);
                let int = self.types().builtin(BuiltinType::Int);
                let error = self.types().builtin(BuiltinType::ParseIntError);
                self.types().intern(TyData::Result { ok: int, error })
            }
            BuiltinValue::DurationMilliseconds => {
                let int = self.types().builtin(BuiltinType::Int);
                self.check_fixed_arguments(expression, arguments, &[int]);
                self.types().builtin(BuiltinType::Duration)
            }
            BuiltinValue::FileOpenRead | BuiltinValue::FileCreate => {
                let text = self.types().builtin(BuiltinType::Text);
                self.check_fixed_arguments(expression, arguments, &[text]);
                let file = self.types().builtin(BuiltinType::File);
                self.types().intern(TyData::Task(file))
            }
            BuiltinValue::SocketConnect => {
                let text = self.types().builtin(BuiltinType::Text);
                let int = self.types().builtin(BuiltinType::Int);
                self.check_fixed_arguments(expression, arguments, &[text, int]);
                let socket = self.types().builtin(BuiltinType::Socket);
                self.types().intern(TyData::Task(socket))
            }
            _ => unreachable!("caller filters standard builtins"),
        }
    }

    fn check_fixed_arguments(
        &mut self,
        expression: ExprId,
        arguments: &[ExprId],
        expected: &[TyId],
    ) {
        if arguments.len() != expected.len() {
            self.call_arity(expression, expected.len(), arguments.len());
        }
        for (argument, ty) in arguments.iter().zip(expected) {
            self.check_expr(*argument, Some(*ty), ExpressionContext::Value);
        }
    }

    fn check_callable_arguments(
        &mut self,
        expression: ExprId,
        signature: &CallableSignature,
        arguments: &[ExprId],
        explicit: &[TyId],
        expected: Option<TyId>,
        mut substitution: Substitution,
    ) -> (TyId, Substitution) {
        if arguments.len() != signature.params.len() {
            self.call_arity(expression, signature.params.len(), arguments.len());
        }
        if explicit.len() > signature.call_generic_params.len() {
            self.error_at(
                "TypeMismatch",
                "too many explicit generic arguments",
                expression,
            );
        }
        for (parameter, ty) in signature.call_generic_params.iter().zip(explicit) {
            substitution.insert(*parameter, *ty);
        }
        if let Some(expected) = expected {
            unify_type(
                &self.analyzer.typed.types,
                signature.return_ty,
                expected,
                &mut substitution,
            );
        }
        let mut actual_types = Vec::new();
        for (argument, (_, parameter_ty)) in arguments.iter().zip(&signature.params) {
            let instantiated = self.types().substitute(*parameter_ty, &substitution);
            let argument_expected =
                (!contains_unbound_param(&self.analyzer.typed.types, instantiated, &substitution))
                    .then_some(instantiated);
            let previous_mode = self.dynamic_coercion_mode;
            if !signature.is_async
                && argument_expected.is_some_and(|ty| {
                    matches!(self.analyzer.typed.types.data(ty), TyData::View { .. })
                })
            {
                self.dynamic_coercion_mode = DynamicCoercionMode::CallBorrow;
            }
            let actual = self.check_expr(*argument, argument_expected, ExpressionContext::Value);
            self.dynamic_coercion_mode = previous_mode;
            unify_type(
                &self.analyzer.typed.types,
                *parameter_ty,
                actual,
                &mut substitution,
            );
            actual_types.push((*argument, actual, *parameter_ty));
        }
        for parameter in &signature.call_generic_params {
            if substitution.get(*parameter).is_none() {
                self.error_at(
                    "CannotInferType",
                    format!(
                        "cannot infer generic parameter `{}`",
                        self.analyzer.program.generic_params[*parameter].name
                    ),
                    expression,
                );
            }
        }
        for (argument, actual, parameter_ty) in actual_types {
            let expected = self.types().substitute(parameter_ty, &substitution);
            let previous_mode = self.dynamic_coercion_mode;
            if !signature.is_async
                && matches!(
                    self.analyzer.typed.types.data(expected),
                    TyData::View { .. }
                )
            {
                self.dynamic_coercion_mode = DynamicCoercionMode::CallBorrow;
            }
            self.coerce(argument, actual, expected);
            self.dynamic_coercion_mode = previous_mode;
        }
        let return_ty = self.types().substitute(signature.return_ty, &substitution);
        (return_ty, substitution)
    }

    #[allow(clippy::too_many_lines)]
    fn check_method_call(
        &mut self,
        expression: ExprId,
        receiver: ExprId,
        method_name: &Name,
        type_arguments: &[TypeRefId],
        arguments: &[ExprId],
        expected: Option<TyId>,
    ) -> TyId {
        if let Expr::Path(type_path) = self.source().expressions[receiver].clone()
            && let Some((variant, owner)) = self.resolve_qualified_variant(&type_path, method_name)
        {
            return self.check_variant_constructor(expression, variant, owner, arguments, expected);
        }
        let previous = self.allow_dirty_self_projection;
        let previous_scoped = self.checking_scoped_receiver;
        self.allow_dirty_self_projection = true;
        self.checking_scoped_receiver = true;
        let receiver_ty = self.check_expr(receiver, None, ExpressionContext::Value);
        self.allow_dirty_self_projection = previous;
        self.checking_scoped_receiver = previous_scoped;
        if let TyData::List(element) = self.types().data(receiver_ty).clone()
            && let Some(result) = self.check_list_method_call(
                expression,
                receiver,
                element,
                method_name,
                type_arguments,
                arguments,
            )
        {
            return result;
        }
        if self.types().data(receiver_ty) == &TyData::Builtin(BuiltinType::TaskFault)
            && let Some(result) = self.check_task_fault_method_call(
                expression,
                method_name,
                type_arguments,
                arguments,
            )
        {
            return result;
        }
        if let TyData::Builtin(builtin) = self.types().data(receiver_ty).clone()
            && let Some(result) = self.check_standard_value_method_call(
                expression,
                receiver,
                builtin,
                method_name,
                type_arguments,
                arguments,
            )
        {
            return result;
        }
        if self.self_dirty
            && self
                .semantics
                .expression_places
                .get(receiver)
                .is_some_and(|place| matches!(place.root, PlaceRoot::SelfValue))
        {
            self.error_at(
                "InvariantIsolationViolation",
                "a mutated receiver cannot be used for a nested method call",
                receiver,
            );
        }
        if let Some(result) = self.check_concept_dot_call(
            expression,
            receiver,
            receiver_ty,
            method_name,
            type_arguments,
            arguments,
            expected,
        ) {
            return result;
        }
        let mut candidates = Vec::new();
        for (implementation, definition) in self.analyzer.program.definitions.iter() {
            let DefinitionKind::InherentImpl(inherent) = &definition.kind else {
                continue;
            };
            let Some(target) = self
                .analyzer
                .typed
                .resolved_type_refs
                .get(inherent.target)
                .copied()
            else {
                continue;
            };
            let mut substitution = Substitution::default();
            if !unify_type(
                &self.analyzer.typed.types,
                target,
                receiver_ty,
                &mut substitution,
            ) {
                continue;
            }
            for method in &inherent.methods {
                if self.analyzer.program.definitions[*method].name.as_ref() == Some(method_name) {
                    candidates.push((implementation, *method, substitution.clone()));
                }
            }
        }
        let mut concept_candidates =
            self.find_concrete_concept_candidates(receiver_ty, method_name);
        let candidate_count = candidates.len() + concept_candidates.len();
        if candidate_count != 1 {
            self.error_at(
                if candidate_count == 0 {
                    "UnknownName"
                } else {
                    "AmbiguousConceptMethod"
                },
                if candidate_count == 0 {
                    format!(
                        "no method `{method_name}` for {}",
                        self.type_name(receiver_ty)
                    )
                } else {
                    format!("multiple methods named `{method_name}` apply")
                },
                expression,
            );
            return self.types().error();
        }
        if let Some((requirement, concept, witness)) = concept_candidates.pop() {
            self.reject_manual_scoped_dispose(requirement, receiver);
            let Some(signature) =
                self.instantiate_concept_signature(requirement, receiver_ty, &concept)
            else {
                return self.types().error();
            };
            if signature.receiver == Some(ReceiverKind::Mutable)
                && self
                    .semantics
                    .expression_places
                    .get(receiver)
                    .is_none_or(|place| place.mutability != Mutability::Mutable)
            {
                self.error_at(
                    "MutReceiverRequiresVar",
                    "mut self concept method requires a mutable receiver place",
                    receiver,
                );
            }
            let explicit = self.resolve_call_type_arguments(type_arguments);
            let (return_ty, substitution) = self.check_callable_arguments(
                expression,
                &signature,
                arguments,
                &explicit,
                expected,
                Substitution::default(),
            );
            self.finish_call_arguments(arguments);
            let witnesses = self.resolve_bound_witnesses(&signature, &substitution, expression);
            let return_ty =
                self.normalize_call_type(return_ty, &signature, &substitution, &witnesses);
            self.semantics.calls.insert(
                expression,
                CallResolution {
                    target: CallTarget::StaticConcept { requirement },
                    substitution,
                    dispatch_witness: Some(witness),
                    witnesses,
                    receiver: Some(if signature.receiver == Some(ReceiverKind::Mutable) {
                        ReceiverPassing::InOut
                    } else {
                        ReceiverPassing::Value
                    }),
                },
            );
            return return_ty;
        }
        let (_, method, initial) = candidates.pop().expect("one candidate");
        let Some(Signature::Callable(signature)) =
            self.analyzer.typed.signatures.get(method).cloned()
        else {
            return self.types().error();
        };
        if signature.receiver == Some(ReceiverKind::Mutable) {
            let mutable_place = self
                .semantics
                .expression_places
                .get(receiver)
                .is_some_and(|place| place.mutability == Mutability::Mutable);
            if !mutable_place {
                self.error_at(
                    "MutReceiverRequiresVar",
                    "mut self method requires a mutable receiver place",
                    receiver,
                );
            }
        }
        let explicit = self.resolve_call_type_arguments(type_arguments);
        let (return_ty, substitution) = self.check_callable_arguments(
            expression, &signature, arguments, &explicit, expected, initial,
        );
        self.finish_call_arguments(arguments);
        let witnesses = self.resolve_bound_witnesses(&signature, &substitution, expression);
        let return_ty = self.normalize_call_type(return_ty, &signature, &substitution, &witnesses);
        if signature.receiver == Some(ReceiverKind::Mutable)
            && let Some(receiver_place) = self.semantics.expression_places.get(receiver).cloned()
        {
            for argument in arguments {
                if self
                    .semantics
                    .expression_places
                    .get(*argument)
                    .is_some_and(|argument_place| places_overlap(&receiver_place, argument_place))
                {
                    self.error_at(
                        "InoutAliasConflict",
                        "an argument aliases the mutable receiver",
                        *argument,
                    );
                }
            }
        }
        self.semantics.calls.insert(
            expression,
            CallResolution {
                target: CallTarget::InherentMethod(method),
                substitution,
                dispatch_witness: None,
                witnesses,
                receiver: Some(if signature.receiver == Some(ReceiverKind::Mutable) {
                    ReceiverPassing::InOut
                } else {
                    ReceiverPassing::Value
                }),
            },
        );
        return_ty
    }

    fn check_list_method_call(
        &mut self,
        expression: ExprId,
        receiver: ExprId,
        element: TyId,
        method_name: &Name,
        type_arguments: &[TypeRefId],
        arguments: &[ExprId],
    ) -> Option<TyId> {
        let (builtin, receiver_passing, result) = match method_name.as_str() {
            "add" => {
                if arguments.len() != 1 {
                    self.call_arity(expression, 1, arguments.len());
                }
                if let Some(argument) = arguments.first() {
                    self.check_expr(*argument, Some(element), ExpressionContext::Value);
                }
                if self
                    .semantics
                    .expression_places
                    .get(receiver)
                    .is_none_or(|place| place.mutability != Mutability::Mutable)
                {
                    self.error_at(
                        "MutReceiverRequiresVar",
                        "List.add requires a mutable `var` receiver",
                        receiver,
                    );
                }
                (
                    BuiltinValue::ListAdd,
                    ReceiverPassing::InOut,
                    self.types().builtin(BuiltinType::Unit),
                )
            }
            "length" => {
                if !arguments.is_empty() {
                    self.call_arity(expression, 0, arguments.len());
                }
                (
                    BuiltinValue::ListLength,
                    ReceiverPassing::Value,
                    self.types().builtin(BuiltinType::Int),
                )
            }
            "get" => {
                let int = self.types().builtin(BuiltinType::Int);
                self.check_fixed_arguments(expression, arguments, &[int]);
                let result = self.types().intern(TyData::Option(element));
                (BuiltinValue::ListGet, ReceiverPassing::Value, result)
            }
            _ => return None,
        };
        if !type_arguments.is_empty() {
            self.error_at(
                "TypeMismatch",
                "List methods do not accept explicit type arguments",
                expression,
            );
        }
        self.finish_call_arguments(arguments);
        self.semantics.calls.insert(
            expression,
            CallResolution {
                target: CallTarget::Builtin(builtin),
                substitution: Substitution::default(),
                dispatch_witness: None,
                witnesses: Vec::new(),
                receiver: Some(receiver_passing),
            },
        );
        Some(result)
    }

    fn check_task_fault_method_call(
        &mut self,
        expression: ExprId,
        method_name: &Name,
        type_arguments: &[TypeRefId],
        arguments: &[ExprId],
    ) -> Option<TyId> {
        let builtin = match method_name.as_str() {
            "code" => BuiltinValue::TaskFaultCode,
            "message" => BuiltinValue::TaskFaultMessage,
            _ => return None,
        };
        if !type_arguments.is_empty() {
            self.error_at(
                "TypeMismatch",
                "TaskFault accessors do not accept explicit type arguments",
                expression,
            );
        }
        self.check_fixed_arguments(expression, arguments, &[]);
        self.finish_call_arguments(arguments);
        self.semantics.calls.insert(
            expression,
            CallResolution {
                target: CallTarget::Builtin(builtin),
                substitution: Substitution::default(),
                dispatch_witness: None,
                witnesses: Vec::new(),
                receiver: Some(ReceiverPassing::Value),
            },
        );
        Some(self.types().builtin(BuiltinType::Text))
    }

    #[allow(clippy::too_many_arguments)]
    fn check_standard_value_method_call(
        &mut self,
        expression: ExprId,
        receiver: ExprId,
        receiver_type: BuiltinType,
        method_name: &Name,
        type_arguments: &[TypeRefId],
        arguments: &[ExprId],
    ) -> Option<TyId> {
        let unit = self.types().builtin(BuiltinType::Unit);
        let text = self.types().builtin(BuiltinType::Text);
        let (builtin, receiver_passing, parameters, result) =
            match (receiver_type, method_name.as_str()) {
                (BuiltinType::Duration, "as_milliseconds") => (
                    BuiltinValue::DurationAsMilliseconds,
                    ReceiverPassing::Value,
                    Vec::new(),
                    self.types().builtin(BuiltinType::Int),
                ),
                (BuiltinType::File, "read_text") => (
                    BuiltinValue::FileReadText,
                    ReceiverPassing::Value,
                    Vec::new(),
                    self.types().intern(TyData::Task(text)),
                ),
                (BuiltinType::File, "write_text") => (
                    BuiltinValue::FileWriteText,
                    ReceiverPassing::Value,
                    vec![text],
                    self.types().intern(TyData::Task(unit)),
                ),
                (BuiltinType::File, "close") => (
                    BuiltinValue::FileClose,
                    ReceiverPassing::InOut,
                    Vec::new(),
                    unit,
                ),
                (BuiltinType::Socket, "read_text") => (
                    BuiltinValue::SocketReadText,
                    ReceiverPassing::Value,
                    Vec::new(),
                    self.types().intern(TyData::Task(text)),
                ),
                (BuiltinType::Socket, "write_text") => (
                    BuiltinValue::SocketWriteText,
                    ReceiverPassing::Value,
                    vec![text],
                    self.types().intern(TyData::Task(unit)),
                ),
                (BuiltinType::Socket, "close") => (
                    BuiltinValue::SocketClose,
                    ReceiverPassing::InOut,
                    Vec::new(),
                    unit,
                ),
                _ => return None,
            };
        if !type_arguments.is_empty() {
            self.error_at(
                "TypeMismatch",
                "standard value methods do not accept explicit type arguments",
                expression,
            );
        }
        self.check_fixed_arguments(expression, arguments, &parameters);
        if receiver_passing == ReceiverPassing::InOut {
            let mutable = self
                .semantics
                .expression_places
                .get(receiver)
                .is_some_and(|place| place.mutability == Mutability::Mutable);
            if !mutable {
                self.error_at(
                    "MutReceiverRequiresVar",
                    "close requires a mutable receiver place",
                    receiver,
                );
            }
            if self
                .semantics
                .expression_places
                .get(receiver)
                .and_then(|place| match place.root {
                    PlaceRoot::Local(local) if place.projections.is_empty() => Some(local),
                    _ => None,
                })
                .is_some_and(|local| self.scoped_locals.contains(&local))
            {
                self.error_at(
                    "ManualDisposeOfScopedValue",
                    "a scoped resource is closed automatically and cannot be closed manually",
                    receiver,
                );
            }
        }
        self.finish_call_arguments(arguments);
        self.semantics.calls.insert(
            expression,
            CallResolution {
                target: CallTarget::Builtin(builtin),
                substitution: Substitution::default(),
                dispatch_witness: None,
                witnesses: Vec::new(),
                receiver: Some(receiver_passing),
            },
        );
        Some(result)
    }

    fn find_concrete_concept_candidates(
        &mut self,
        receiver_ty: TyId,
        method_name: &Name,
    ) -> Vec<(DefId, ConceptInstance, WitnessSelection)> {
        let module = self.analyzer.program.definitions[self.environment.owner].module;
        let concepts = self
            .analyzer
            .def_maps
            .map(module)
            .into_iter()
            .flat_map(|map| map.entries(Namespace::Concept))
            .filter_map(|(_, binding)| binding.unique())
            .collect::<Vec<_>>();
        let environment = ParamEnv {
            bounds: self
                .environment
                .bounds
                .iter()
                .cloned()
                .map(|bound| Goal {
                    self_ty: bound.self_ty,
                    concept: bound.concept,
                })
                .collect(),
        };
        let mut candidates = Vec::new();
        for concept in concepts {
            let Some(requirement) = self.concept_method(concept, method_name) else {
                continue;
            };
            let instance = ConceptInstance {
                concept,
                bindings: Vec::new(),
            };
            let goal = Goal {
                self_ty: receiver_ty,
                concept: instance,
            };
            let index = &self.analyzer.impl_index;
            let types = &mut self.analyzer.typed.types;
            let mut solver = crate::ConformanceSolver::new(index, types);
            if let Ok(witness) = solver.solve(&goal, &environment) {
                let instance = ConceptInstance {
                    concept,
                    bindings: witness.associated_types.clone(),
                };
                candidates.push((requirement, instance, witness));
            }
        }
        candidates
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn check_concept_dot_call(
        &mut self,
        expression: ExprId,
        receiver_expression: ExprId,
        receiver_ty: TyId,
        method_name: &Name,
        type_arguments: &[TypeRefId],
        arguments: &[ExprId],
        expected: Option<TyId>,
    ) -> Option<TyId> {
        if let TyData::View { mutability, target } =
            self.analyzer.typed.types.data(receiver_ty).clone()
        {
            let TyData::DynTarget(instance) = self.analyzer.typed.types.data(target).clone() else {
                return None;
            };
            let requirement = self.concept_method(instance.concept, method_name)?;
            let signature =
                self.instantiate_concept_signature(requirement, receiver_ty, &instance)?;
            if signature.receiver == Some(ReceiverKind::Mutable)
                && mutability != Mutability::Mutable
            {
                self.error_at(
                    "DynMutReceiverUnavailable",
                    "a readonly interface argument cannot call a `mut self` method",
                    receiver_expression,
                );
            }
            if signature.receiver == Some(ReceiverKind::Mutable)
                && self
                    .semantics
                    .expression_places
                    .get(receiver_expression)
                    .is_some_and(|place| {
                        matches!(place.root, PlaceRoot::Local(_))
                            && place.mutability != Mutability::Mutable
                    })
            {
                self.error_at(
                    "MutReceiverRequiresVar",
                    "a stored interface value must use `var` before calling a `mut self` method",
                    receiver_expression,
                );
            }
            let explicit = self.resolve_call_type_arguments(type_arguments);
            let (return_ty, substitution) = self.check_callable_arguments(
                expression,
                &signature,
                arguments,
                &explicit,
                expected,
                Substitution::default(),
            );
            self.finish_call_arguments(arguments);
            let witnesses = self.resolve_bound_witnesses(&signature, &substitution, expression);
            let return_ty =
                self.normalize_call_type(return_ty, &signature, &substitution, &witnesses);
            self.semantics.calls.insert(
                expression,
                CallResolution {
                    target: CallTarget::DynamicConcept { requirement },
                    substitution,
                    dispatch_witness: None,
                    witnesses,
                    receiver: Some(if signature.receiver == Some(ReceiverKind::Mutable) {
                        ReceiverPassing::InOut
                    } else {
                        ReceiverPassing::Value
                    }),
                },
            );
            return Some(return_ty);
        }

        let mut candidates = self
            .environment
            .bounds
            .iter()
            .enumerate()
            .filter_map(|(index, bound)| {
                (bound.self_ty == receiver_ty)
                    .then(|| {
                        self.concept_method(bound.concept.concept, method_name)
                            .map(|requirement| (index, bound.clone(), requirement))
                    })
                    .flatten()
            })
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return None;
        }
        if candidates.len() > 1 {
            self.error_at(
                "AmbiguousConceptMethod",
                format!("multiple bounded concepts provide `{method_name}`"),
                expression,
            );
            return Some(self.types().error());
        }
        let (bound_index, bound, requirement) = candidates.pop().expect("one candidate");
        let signature =
            self.instantiate_concept_signature(requirement, receiver_ty, &bound.concept)?;
        let explicit = self.resolve_call_type_arguments(type_arguments);
        let (return_ty, substitution) = self.check_callable_arguments(
            expression,
            &signature,
            arguments,
            &explicit,
            expected,
            Substitution::default(),
        );
        self.finish_call_arguments(arguments);
        let witnesses = self.resolve_bound_witnesses(&signature, &substitution, expression);
        let return_ty = self.normalize_call_type(return_ty, &signature, &substitution, &witnesses);
        let witness = WitnessSelection {
            source: WitnessSource::ParamBound(bound_index),
            substitution: Substitution::default(),
            associated_types: bound.concept.bindings.clone(),
            prerequisites: Vec::new(),
        };
        self.semantics.calls.insert(
            expression,
            CallResolution {
                target: CallTarget::StaticConcept { requirement },
                substitution,
                dispatch_witness: Some(witness),
                witnesses,
                receiver: Some(if signature.receiver == Some(ReceiverKind::Mutable) {
                    ReceiverPassing::InOut
                } else {
                    ReceiverPassing::Value
                }),
            },
        );
        Some(return_ty)
    }

    fn instantiate_concept_signature(
        &mut self,
        requirement: DefId,
        self_ty: TyId,
        concept: &ConceptInstance,
    ) -> Option<CallableSignature> {
        let Signature::Callable(signature) =
            self.analyzer.typed.signatures.get(requirement)?.clone()
        else {
            return None;
        };
        Some(CallableSignature {
            is_async: signature.is_async,
            generic_params: signature.generic_params,
            call_generic_params: signature.call_generic_params,
            receiver: signature.receiver,
            params: signature
                .params
                .into_iter()
                .map(|(parameter, ty)| {
                    (
                        parameter,
                        self.instantiate_concept_type(ty, self_ty, concept),
                    )
                })
                .collect(),
            return_ty: self.instantiate_concept_type(signature.return_ty, self_ty, concept),
            bounds: self.instantiate_concept_bounds(&signature.bounds, self_ty, concept),
            call_bounds: self.instantiate_concept_bounds(&signature.call_bounds, self_ty, concept),
        })
    }

    fn instantiate_concept_bounds(
        &mut self,
        bounds: &[Bound],
        self_ty: TyId,
        concept: &ConceptInstance,
    ) -> Vec<Bound> {
        bounds
            .iter()
            .map(|bound| Bound {
                self_ty: self.instantiate_concept_type(bound.self_ty, self_ty, concept),
                concept: ConceptInstance {
                    concept: bound.concept.concept,
                    bindings: bound
                        .concept
                        .bindings
                        .iter()
                        .map(|binding| AssociatedTypeBinding {
                            associated_type: binding.associated_type,
                            ty: self.instantiate_concept_type(binding.ty, self_ty, concept),
                        })
                        .collect(),
                },
            })
            .collect()
    }

    fn instantiate_concept_type(
        &mut self,
        ty: TyId,
        concrete_self: TyId,
        instance: &ConceptInstance,
    ) -> TyId {
        match self.analyzer.typed.types.data(ty).clone() {
            TyData::SelfType(concept) if concept == instance.concept => concrete_self,
            TyData::Tuple(elements) => {
                let elements = elements
                    .into_iter()
                    .map(|element| self.instantiate_concept_type(element, concrete_self, instance))
                    .collect();
                self.types().intern(TyData::Tuple(elements))
            }
            TyData::List(element) => {
                let element = self.instantiate_concept_type(element, concrete_self, instance);
                self.types().intern(TyData::List(element))
            }
            TyData::Projection {
                concept,
                associated_type,
                ..
            } if concept == instance.concept => {
                if let Some(binding) = instance
                    .bindings
                    .iter()
                    .find(|binding| binding.associated_type == associated_type)
                {
                    binding.ty
                } else {
                    self.types().intern(TyData::Projection {
                        self_ty: concrete_self,
                        concept,
                        associated_type,
                    })
                }
            }
            TyData::Option(element) => {
                let element = self.instantiate_concept_type(element, concrete_self, instance);
                self.types().intern(TyData::Option(element))
            }
            TyData::Result { ok, error } => {
                let ok = self.instantiate_concept_type(ok, concrete_self, instance);
                let error = self.instantiate_concept_type(error, concrete_self, instance);
                self.types().intern(TyData::Result { ok, error })
            }
            TyData::Task(output) => {
                let output = self.instantiate_concept_type(output, concrete_self, instance);
                self.types().intern(TyData::Task(output))
            }
            TyData::TaskOutcome(output) => {
                let output = self.instantiate_concept_type(output, concrete_self, instance);
                self.types().intern(TyData::TaskOutcome(output))
            }
            TyData::Nominal {
                definition,
                arguments,
            } => {
                let arguments = arguments
                    .into_iter()
                    .map(|argument| {
                        self.instantiate_concept_type(argument, concrete_self, instance)
                    })
                    .collect();
                self.types().intern(TyData::Nominal {
                    definition,
                    arguments,
                })
            }
            _ => ty,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn check_qualified_call(
        &mut self,
        expression: ExprId,
        self_ty: TypeRefId,
        concept: &loom_hir::ConceptRef,
        method: &Name,
        type_arguments: &[TypeRefId],
        arguments: &[ExprId],
        expected: Option<TyId>,
    ) -> TyId {
        let context = self.analyzer.type_context(self.environment.owner);
        let self_ty = self.analyzer.resolve_type_ref(self_ty, &context);
        let Some(concept_instance) = self.analyzer.resolve_concept_ref(concept, &context, false)
        else {
            return self.types().error();
        };
        let Some(requirement) = self.concept_method(concept_instance.concept, method) else {
            self.error_at(
                "UnknownName",
                format!("concept has no method `{method}`"),
                expression,
            );
            return self.types().error();
        };
        let Some(witness) = self.solve_witness(self_ty, concept_instance.clone(), expression)
        else {
            return self.types().error();
        };
        let concept_instance = ConceptInstance {
            concept: concept_instance.concept,
            bindings: witness.associated_types.clone(),
        };
        let Some(signature) =
            self.instantiate_concept_signature(requirement, self_ty, &concept_instance)
        else {
            return self.types().error();
        };
        let (call_arguments, receiver) = if signature.receiver == Some(ReceiverKind::Static) {
            (arguments, None)
        } else if let Some((receiver, rest)) = arguments.split_first() {
            self.check_expr(*receiver, Some(self_ty), ExpressionContext::Value);
            if signature.receiver == Some(ReceiverKind::Mutable)
                && self
                    .semantics
                    .expression_places
                    .get(*receiver)
                    .is_none_or(|place| place.mutability != Mutability::Mutable)
            {
                self.error_at(
                    "MutReceiverRequiresVar",
                    "qualified mut self call requires a mutable receiver place",
                    *receiver,
                );
            }
            (rest, Some(*receiver))
        } else {
            self.call_arity(expression, signature.params.len() + 1, 0);
            return self.types().error();
        };
        if let Some(receiver) = receiver {
            self.reject_manual_scoped_dispose(requirement, receiver);
        }
        let explicit = self.resolve_call_type_arguments(type_arguments);
        let (return_ty, substitution) = self.check_callable_arguments(
            expression,
            &signature,
            call_arguments,
            &explicit,
            expected,
            Substitution::default(),
        );
        self.finish_call_arguments(call_arguments);
        let witnesses = self.resolve_bound_witnesses(&signature, &substitution, expression);
        let return_ty = self.normalize_call_type(return_ty, &signature, &substitution, &witnesses);
        self.semantics.calls.insert(
            expression,
            CallResolution {
                target: CallTarget::StaticConcept { requirement },
                substitution,
                dispatch_witness: Some(witness),
                witnesses,
                receiver: receiver.map(|_| {
                    if signature.receiver == Some(ReceiverKind::Mutable) {
                        ReceiverPassing::InOut
                    } else {
                        ReceiverPassing::Value
                    }
                }),
            },
        );
        return_ty
    }

    fn reject_manual_scoped_dispose(&mut self, requirement: DefId, receiver: ExprId) {
        let Some(dispose) = self.language_concept(DISPOSE_CONCEPT) else {
            return;
        };
        if self.concept_method(dispose, &Name::new("dispose")) != Some(requirement) {
            return;
        }
        let Some(place) = self.semantics.expression_places.get(receiver) else {
            return;
        };
        let PlaceRoot::Local(local) = place.root else {
            return;
        };
        if place.projections.is_empty() && self.scoped_locals.contains(&local) {
            self.error_at(
                "ManualDisposeOfScopedValue",
                "a scoped resource is disposed automatically and cannot be disposed manually",
                receiver,
            );
        }
    }

    fn check_variant_constructor(
        &mut self,
        expression: ExprId,
        variant: DefId,
        owner: DefId,
        arguments: &[ExprId],
        expected: Option<TyId>,
    ) -> TyId {
        let generic_params = self.analyzer.type_generic_params(owner);
        let mut substitution = Substitution::default();
        if let Some(expected) = expected {
            let pattern = self.analyzer.nominal_self_type(owner);
            unify_type(
                &self.analyzer.typed.types,
                pattern,
                expected,
                &mut substitution,
            );
        }
        let Some(Signature::Variant { payload, .. }) =
            self.analyzer.typed.signatures.get(variant).cloned()
        else {
            return self.types().error();
        };
        if payload.len() != arguments.len() {
            self.call_arity(expression, payload.len(), arguments.len());
        }
        let mut actuals = Vec::new();
        for (argument, payload_ty) in arguments.iter().zip(&payload) {
            let instantiated = self.types().substitute(*payload_ty, &substitution);
            let expected =
                (!contains_unbound_param(&self.analyzer.typed.types, instantiated, &substitution))
                    .then_some(instantiated);
            let actual = self.check_expr(*argument, expected, ExpressionContext::Value);
            unify_type(
                &self.analyzer.typed.types,
                *payload_ty,
                actual,
                &mut substitution,
            );
            actuals.push((*argument, actual, *payload_ty));
        }
        let mut type_arguments = Vec::new();
        for parameter in generic_params {
            if let Some(argument) = substitution.get(parameter) {
                type_arguments.push(argument);
            } else {
                self.error_at(
                    "CannotInferType",
                    "cannot infer enum type arguments",
                    expression,
                );
                type_arguments.push(self.types().error());
            }
        }
        for (argument, actual, payload_ty) in actuals {
            let expected = self.types().substitute(payload_ty, &substitution);
            self.coerce(argument, actual, expected);
        }
        let result = self.types().intern(TyData::Nominal {
            definition: owner,
            arguments: type_arguments,
        });
        self.semantics.calls.insert(
            expression,
            CallResolution {
                target: CallTarget::EnumVariant(variant),
                substitution,
                dispatch_witness: None,
                witnesses: Vec::new(),
                receiver: None,
            },
        );
        result
    }

    #[allow(clippy::too_many_lines)]
    fn check_record_literal(
        &mut self,
        expression: ExprId,
        path: &Path,
        fields: &[loom_hir::RecordFieldValue],
        expected: Option<TyId>,
    ) -> TyId {
        let module = self.analyzer.program.definitions[self.environment.owner].module;
        let resolver = crate::Resolver::new(self.analyzer.program, self.analyzer.def_maps, module);
        let Ok(definition) = resolver.resolve_definition(path, Namespace::Type) else {
            self.error_at(
                "UnknownName",
                format!("unknown record `{}`", path.as_string()),
                expression,
            );
            return self.types().error();
        };
        let DefinitionKind::Record(record) = &self.analyzer.program.definitions[definition].kind
        else {
            self.error_at("TypeMismatch", "literal target is not a record", expression);
            return self.types().error();
        };
        let record = record.clone();
        let mut substitution = Substitution::default();
        if let Some(expected) = expected {
            let pattern = self.analyzer.nominal_self_type(definition);
            unify_type(
                &self.analyzer.typed.types,
                pattern,
                expected,
                &mut substitution,
            );
        }
        let mut source_values = BTreeMap::<Name, (ExprId, Span)>::new();
        for field in fields {
            if let Some((_, previous_span)) =
                source_values.insert(field.name.clone(), (field.value, field.span))
            {
                self.analyzer.diagnostics.push(
                    Diagnostic::error(
                        "DuplicateField",
                        format!("field `{}` is supplied more than once", field.name),
                        field.span,
                    )
                    .with_label(previous_span, "first value"),
                );
            }
        }
        let mut canonical = Vec::new();
        let mut actuals = Vec::new();
        for field in &record.fields {
            let name = self.analyzer.program.definitions[*field]
                .name
                .clone()
                .unwrap_or_else(|| Name::new("<error>"));
            let Some((value, _)) = source_values.remove(&name) else {
                self.error_at(
                    "MissingField",
                    format!("missing field `{name}`"),
                    expression,
                );
                continue;
            };
            let Some(Signature::Field { ty, .. }) =
                self.analyzer.typed.signatures.get(*field).cloned()
            else {
                continue;
            };
            let instantiated = self.types().substitute(ty, &substitution);
            let field_expected =
                (!contains_unbound_param(&self.analyzer.typed.types, instantiated, &substitution))
                    .then_some(instantiated);
            let actual = self.check_expr(value, field_expected, ExpressionContext::Value);
            unify_type(&self.analyzer.typed.types, ty, actual, &mut substitution);
            actuals.push((value, actual, ty));
            canonical.push((*field, value));
        }
        for (unknown, (_, span)) in source_values {
            self.error(
                "UnknownField",
                format!("record has no field `{unknown}`"),
                span,
            );
        }
        let mut arguments = Vec::new();
        for parameter in &record.generic_params {
            if let Some(argument) = substitution.get(*parameter) {
                arguments.push(argument);
            } else {
                self.error_at(
                    "CannotInferType",
                    format!(
                        "cannot infer record parameter `{}`",
                        self.analyzer.program.generic_params[*parameter].name
                    ),
                    expression,
                );
                arguments.push(self.types().error());
            }
        }
        for (value, actual, field_ty) in actuals {
            let expected = self.types().substitute(field_ty, &substitution);
            self.coerce(value, actual, expected);
        }
        self.semantics.record_fields.insert(expression, canonical);
        let nominal = self.types().intern(TyData::Nominal {
            definition,
            arguments,
        });
        if record.invariant.is_some() {
            let violation = self.types().builtin(BuiltinType::Violation);
            self.types().intern(TyData::Result {
                ok: nominal,
                error: violation,
            })
        } else {
            nominal
        }
    }

    fn check_view(
        &mut self,
        expression: ExprId,
        mutable: bool,
        concept: &loom_hir::ConceptRef,
        source: ExprId,
    ) -> TyId {
        let previous_view_source = self.checking_view_source;
        self.checking_view_source = true;
        let owner_ty = self.check_expr(source, None, ExpressionContext::Value);
        self.checking_view_source = previous_view_source;
        let owner = self.semantics.expression_places.get(source).cloned();
        if owner.is_none() {
            self.error_at(
                "IllegalDynConversion",
                "interface adaptation requires a concrete argument place",
                source,
            );
        }
        if mutable
            && self
                .semantics
                .expression_places
                .get(source)
                .is_none_or(|place| place.mutability != Mutability::Mutable)
        {
            self.error_at(
                "DynMutReceiverUnavailable",
                "a concept with `mut self` requires a mutable argument place",
                source,
            );
        }
        if matches!(
            self.analyzer.typed.types.data(owner_ty),
            TyData::View { .. }
        ) {
            self.error_at(
                "IllegalDynConversion",
                "an erased interface argument cannot be adapted a second time",
                source,
            );
        }
        let context = self.analyzer.type_context(self.environment.owner);
        let target_instance = self.analyzer.resolve_concept_ref(concept, &context, true);
        let target = if let Some(target) = &target_instance {
            self.types().intern(TyData::DynTarget(target.clone()))
        } else {
            self.types().error()
        };
        if let (Some(owner), Some(target_instance)) = (owner, target_instance)
            && let Some(witness) = self.solve_witness(owner_ty, target_instance.clone(), expression)
        {
            let region = *self.regions.last().expect("body has a lexical region");
            let token = ViewTokenId(self.next_view_token);
            self.next_view_token = self.next_view_token.saturating_add(1);
            self.register_borrow(
                owner.clone(),
                mutable,
                region,
                token,
                self.expr_span(expression),
                expression,
            );
            self.semantics.views.insert(
                expression,
                ViewResolution {
                    source: ViewSource::Concrete {
                        witness,
                        writeback: Some(owner),
                    },
                    mutable,
                    region,
                    token,
                },
            );
        }
        self.types().intern(TyData::View {
            mutability: if mutable {
                Mutability::Mutable
            } else {
                Mutability::ReadOnly
            },
            target,
        })
    }

    fn solve_witness(
        &mut self,
        self_ty: TyId,
        concept: ConceptInstance,
        expression: ExprId,
    ) -> Option<WitnessSelection> {
        let environment = ParamEnv {
            bounds: self
                .environment
                .bounds
                .iter()
                .cloned()
                .map(|bound| Goal {
                    self_ty: bound.self_ty,
                    concept: bound.concept,
                })
                .collect(),
        };
        let goal = Goal { self_ty, concept };
        let index = &self.analyzer.impl_index;
        let types = &mut self.analyzer.typed.types;
        let mut solver = crate::ConformanceSolver::new(index, types);
        match solver.solve(&goal, &environment) {
            Ok(witness) => Some(witness),
            Err(failure) => {
                let (code, message) = match failure {
                    SolveFailure::Missing => (
                        "MissingConformance",
                        "no conformance satisfies this dyn construction".to_owned(),
                    ),
                    SolveFailure::Ambiguous(_) => (
                        "DuplicateConformance",
                        "multiple conformances satisfy this dyn construction".to_owned(),
                    ),
                    SolveFailure::Cycle(_) => (
                        "ConformanceResolutionCycle",
                        "conformance resolution entered a cycle".to_owned(),
                    ),
                    SolveFailure::AssociatedTypeMismatch { .. } => (
                        "DynAssociatedTypeMismatch",
                        "conformance associated binding differs from the dyn target".to_owned(),
                    ),
                };
                self.error_at(code, message, expression);
                None
            }
        }
    }

    fn language_concept(&self, name: &str) -> Option<DefId> {
        self.analyzer
            .program
            .definitions
            .iter()
            .find_map(|(definition, item)| {
                let module = &self.analyzer.program.modules[item.module];
                (module.name.as_str() == RESOURCE_MODULE
                    && item
                        .name
                        .as_ref()
                        .is_some_and(|candidate| candidate.as_str() == name)
                    && matches!(item.kind, DefinitionKind::Concept(_)))
                .then_some(definition)
            })
    }

    fn solve_resource_witness(
        &mut self,
        self_ty: TyId,
        concept: ConceptInstance,
    ) -> Option<WitnessSelection> {
        let environment = ParamEnv {
            bounds: self
                .environment
                .bounds
                .iter()
                .cloned()
                .map(|bound| Goal {
                    self_ty: bound.self_ty,
                    concept: bound.concept,
                })
                .collect(),
        };
        let goal = Goal { self_ty, concept };
        let mut solver = crate::ConformanceSolver::new(
            &self.analyzer.impl_index,
            &mut self.analyzer.typed.types,
        );
        solver.solve(&goal, &environment).ok()
    }

    fn has_marker_conformance(&mut self, ty: TyId, marker: &str) -> Option<WitnessSelection> {
        let concept = self.language_concept(marker)?;
        self.solve_resource_witness(
            ty,
            ConceptInstance {
                concept,
                bindings: Vec::new(),
            },
        )
    }

    fn has_marker_obligation(&mut self, ty: TyId, marker: &str) -> bool {
        if marker == MUST_SCOPE_CONCEPT
            && matches!(
                self.types().data(ty),
                TyData::Builtin(BuiltinType::File | BuiltinType::Socket)
            )
        {
            return true;
        }
        self.has_marker_conformance(ty, marker).is_some()
    }

    fn resolve_bound_witnesses(
        &mut self,
        signature: &CallableSignature,
        substitution: &Substitution,
        expression: ExprId,
    ) -> Vec<WitnessSelection> {
        let mut witnesses = Vec::new();
        for bound in &signature.bounds {
            let self_ty = self.types().substitute(bound.self_ty, substitution);
            let bindings = bound
                .concept
                .bindings
                .iter()
                .map(|binding| AssociatedTypeBinding {
                    associated_type: binding.associated_type,
                    ty: self.types().substitute(binding.ty, substitution),
                })
                .collect();
            let concept = ConceptInstance {
                concept: bound.concept.concept,
                bindings,
            };
            if let Some(witness) = self.solve_witness(self_ty, concept, expression) {
                witnesses.push(witness);
            }
        }
        witnesses
    }

    fn normalize_call_type(
        &mut self,
        ty: TyId,
        signature: &CallableSignature,
        substitution: &Substitution,
        witnesses: &[WitnessSelection],
    ) -> TyId {
        let evidence = signature
            .bounds
            .iter()
            .zip(witnesses)
            .map(|(bound, witness)| {
                (
                    self.types().substitute(bound.self_ty, substitution),
                    bound.concept.concept,
                    witness.associated_types.clone(),
                )
            })
            .collect::<Vec<_>>();
        self.normalize_type_with_evidence(ty, &evidence)
    }

    fn normalize_type_with_evidence(
        &mut self,
        ty: TyId,
        evidence: &[(TyId, DefId, Vec<AssociatedTypeBinding>)],
    ) -> TyId {
        match self.analyzer.typed.types.data(ty).clone() {
            TyData::Projection {
                self_ty,
                concept,
                associated_type,
            } => {
                let self_ty = self.normalize_type_with_evidence(self_ty, evidence);
                if let Some(binding) = evidence
                    .iter()
                    .find(|(target, candidate, _)| *target == self_ty && *candidate == concept)
                    .and_then(|(_, _, bindings)| {
                        bindings
                            .iter()
                            .find(|binding| binding.associated_type == associated_type)
                    })
                {
                    self.normalize_type_with_evidence(binding.ty, evidence)
                } else {
                    self.types().intern(TyData::Projection {
                        self_ty,
                        concept,
                        associated_type,
                    })
                }
            }
            TyData::Tuple(elements) => {
                let elements = elements
                    .into_iter()
                    .map(|element| self.normalize_type_with_evidence(element, evidence))
                    .collect();
                self.types().intern(TyData::Tuple(elements))
            }
            TyData::List(element) => {
                let element = self.normalize_type_with_evidence(element, evidence);
                self.types().intern(TyData::List(element))
            }
            TyData::Option(element) => {
                let element = self.normalize_type_with_evidence(element, evidence);
                self.types().intern(TyData::Option(element))
            }
            TyData::Result { ok, error } => {
                let ok = self.normalize_type_with_evidence(ok, evidence);
                let error = self.normalize_type_with_evidence(error, evidence);
                self.types().intern(TyData::Result { ok, error })
            }
            TyData::Task(output) => {
                let output = self.normalize_type_with_evidence(output, evidence);
                self.types().intern(TyData::Task(output))
            }
            TyData::TaskOutcome(output) => {
                let output = self.normalize_type_with_evidence(output, evidence);
                self.types().intern(TyData::TaskOutcome(output))
            }
            TyData::Nominal {
                definition,
                arguments,
            } => {
                let arguments = arguments
                    .into_iter()
                    .map(|argument| self.normalize_type_with_evidence(argument, evidence))
                    .collect();
                self.types().intern(TyData::Nominal {
                    definition,
                    arguments,
                })
            }
            TyData::DynTarget(instance) => {
                let bindings = instance
                    .bindings
                    .into_iter()
                    .map(|binding| AssociatedTypeBinding {
                        associated_type: binding.associated_type,
                        ty: self.normalize_type_with_evidence(binding.ty, evidence),
                    })
                    .collect();
                self.types().intern(TyData::DynTarget(ConceptInstance {
                    concept: instance.concept,
                    bindings,
                }))
            }
            TyData::View { mutability, target } => {
                let target = self.normalize_type_with_evidence(target, evidence);
                self.types().intern(TyData::View { mutability, target })
            }
            TyData::Error
            | TyData::Never
            | TyData::Builtin(_)
            | TyData::Param(_)
            | TyData::SelfType(_) => ty,
        }
    }

    fn resolve_call_type_arguments(&mut self, arguments: &[TypeRefId]) -> Vec<TyId> {
        let context = self.analyzer.type_context(self.environment.owner);
        arguments
            .iter()
            .map(|argument| self.analyzer.resolve_type_ref(*argument, &context))
            .collect()
    }

    fn resolve_qualified_variant(&self, type_path: &Path, name: &Name) -> Option<(DefId, DefId)> {
        let module = self.analyzer.program.definitions[self.environment.owner].module;
        let resolver = crate::Resolver::new(self.analyzer.program, self.analyzer.def_maps, module);
        let owner = resolver
            .resolve_definition(type_path, Namespace::Type)
            .ok()?;
        let DefinitionKind::Enum(enumeration) = &self.analyzer.program.definitions[owner].kind
        else {
            return None;
        };
        enumeration
            .variants
            .iter()
            .copied()
            .find(|variant| self.analyzer.program.definitions[*variant].name.as_ref() == Some(name))
            .map(|variant| (variant, owner))
    }

    fn resolve_pattern_variant(
        &self,
        path: &Path,
        payload: &[PatternId],
        expected: TyId,
    ) -> Option<PatternVariant> {
        let name = path.last()?;
        match self.analyzer.typed.types.data(expected) {
            TyData::Option(_) => match name.as_str() {
                "None" if payload.is_empty() => Some(PatternVariant::None),
                "Some" => Some(PatternVariant::Some),
                _ => None,
            },
            TyData::Result { .. } => match name.as_str() {
                "Ok" => Some(PatternVariant::Ok),
                "Err" => Some(PatternVariant::Err),
                _ => None,
            },
            TyData::TaskOutcome(_) => match name.as_str() {
                "Completed" => Some(PatternVariant::TaskCompleted),
                "Faulted" => Some(PatternVariant::TaskFaulted),
                "Cancelled" if payload.is_empty() => Some(PatternVariant::TaskCancelled),
                _ => None,
            },
            TyData::Builtin(BuiltinType::ParseFloatError) => match name.as_str() {
                "InvalidSyntax" if payload.is_empty() => {
                    Some(PatternVariant::ParseFloatInvalidSyntax)
                }
                "OutOfRange" if payload.is_empty() => Some(PatternVariant::ParseFloatOutOfRange),
                _ => None,
            },
            TyData::Builtin(BuiltinType::ParseIntError) => match name.as_str() {
                "InvalidSyntax" if payload.is_empty() => {
                    Some(PatternVariant::ParseIntInvalidSyntax)
                }
                "OutOfRange" if payload.is_empty() => Some(PatternVariant::ParseIntOutOfRange),
                _ => None,
            },
            TyData::Nominal { definition, .. } => {
                let DefinitionKind::Enum(enumeration) =
                    &self.analyzer.program.definitions[*definition].kind
                else {
                    return None;
                };
                enumeration
                    .variants
                    .iter()
                    .copied()
                    .find(|variant| {
                        self.analyzer.program.definitions[*variant].name.as_ref() == Some(name)
                    })
                    .map(PatternVariant::User)
            }
            _ => None,
        }
    }

    fn variant_payload(&mut self, variant: PatternVariant, expected: TyId) -> Vec<TyId> {
        match (variant, self.analyzer.typed.types.data(expected).clone()) {
            (PatternVariant::Some, TyData::Option(element)) => vec![element],
            (PatternVariant::Ok, TyData::Result { ok, .. }) => vec![ok],
            (PatternVariant::Err, TyData::Result { error, .. }) => vec![error],
            (PatternVariant::TaskCompleted, TyData::TaskOutcome(output)) => vec![output],
            (PatternVariant::TaskFaulted, TyData::TaskOutcome(_)) => {
                vec![self.types().builtin(BuiltinType::TaskFault)]
            }
            (
                PatternVariant::User(variant),
                TyData::Nominal {
                    definition,
                    arguments,
                },
            ) => {
                let Some(Signature::Variant { payload, .. }) =
                    self.analyzer.typed.signatures.get(variant).cloned()
                else {
                    return Vec::new();
                };
                let parameters = self.analyzer.type_generic_params(definition);
                let substitution = substitution_for(&parameters, &arguments);
                payload
                    .into_iter()
                    .map(|ty| self.types().substitute(ty, &substitution))
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    fn check_exhaustive(
        &mut self,
        scrutinee: TyId,
        coverage: &PatternCoverage,
        expression: ExprId,
    ) {
        if coverage.catch_all {
            return;
        }
        let exhaustive = match self.analyzer.typed.types.data(scrutinee) {
            TyData::Builtin(BuiltinType::Bool) => coverage.bool_values.len() == 2,
            TyData::Option(_) => {
                coverage.variants.contains(&PatternVariant::None)
                    && coverage.variants.contains(&PatternVariant::Some)
            }
            TyData::Result { .. } => {
                coverage.variants.contains(&PatternVariant::Ok)
                    && coverage.variants.contains(&PatternVariant::Err)
            }
            TyData::TaskOutcome(_) => {
                coverage.variants.contains(&PatternVariant::TaskCompleted)
                    && coverage.variants.contains(&PatternVariant::TaskFaulted)
                    && coverage.variants.contains(&PatternVariant::TaskCancelled)
            }
            TyData::Builtin(BuiltinType::ParseFloatError) => {
                coverage
                    .variants
                    .contains(&PatternVariant::ParseFloatInvalidSyntax)
                    && coverage
                        .variants
                        .contains(&PatternVariant::ParseFloatOutOfRange)
            }
            TyData::Builtin(BuiltinType::ParseIntError) => {
                coverage
                    .variants
                    .contains(&PatternVariant::ParseIntInvalidSyntax)
                    && coverage
                        .variants
                        .contains(&PatternVariant::ParseIntOutOfRange)
            }
            TyData::Nominal { definition, .. } => {
                if let DefinitionKind::Enum(enumeration) =
                    &self.analyzer.program.definitions[*definition].kind
                {
                    enumeration
                        .variants
                        .iter()
                        .all(|variant| coverage.variants.contains(&PatternVariant::User(*variant)))
                } else {
                    false
                }
            }
            _ => false,
        };
        if !exhaustive {
            self.error_at(
                "NonExhaustiveMatch",
                "match does not cover every possible value",
                expression,
            );
        }
    }

    fn expect_compatible(&mut self, pattern: PatternId, actual: TyId, expected: TyId) {
        if !self.assignable(actual, expected) && !self.assignable(expected, actual) {
            self.error(
                "TypeMismatch",
                format!(
                    "pattern has type {}, expected {}",
                    self.type_name(actual),
                    self.type_name(expected)
                ),
                self.pattern_span(pattern),
            );
        }
    }

    fn concept_method(&self, concept: DefId, name: &Name) -> Option<DefId> {
        let DefinitionKind::Concept(concept) = &self.analyzer.program.definitions[concept].kind
        else {
            return None;
        };
        concept.requirements.iter().copied().find(|requirement| {
            self.analyzer.program.definitions[*requirement]
                .name
                .as_ref()
                == Some(name)
        })
    }

    fn call_arity(&mut self, expression: ExprId, expected: usize, actual: usize) {
        self.error_at(
            "TypeMismatch",
            format!("call expects {expected} arguments, found {actual}"),
            expression,
        );
    }

    fn validate_contract_shape(&mut self, expression: ExprId, source: &Expr) {
        if self.environment.contract == ContractMode::None {
            return;
        }
        let valid = match source {
            Expr::Error
            | Expr::Literal(_)
            | Expr::Path(_)
            | Expr::SelfValue
            | Expr::ResultValue
            | Expr::Old(_)
            | Expr::Field { .. }
            | Expr::Unary { .. }
            | Expr::Binary { .. }
            | Expr::Match { .. } => true,
            Expr::Call { callee, .. } => matches!(
                &self.source().expressions[*callee],
                Expr::Path(path)
                    if path.segments.len() == 1
                        && path.segments[0].name.as_str() == "is_finite"
                        && self.builtin_is_imported("standard.float.is_finite")
            ),
            Expr::Tuple(_)
            | Expr::List(_)
            | Expr::Block { .. }
            | Expr::If { .. }
            | Expr::MethodCall { .. }
            | Expr::QualifiedMethodCall { .. }
            | Expr::Assign { .. }
            | Expr::RecordLiteral { .. }
            | Expr::View { .. }
            | Expr::Await(_)
            | Expr::Sleep(_)
            | Expr::WaitFd { .. }
            | Expr::TaskJoin { .. }
            | Expr::Propagate(_)
            | Expr::Return(_) => false,
        };
        if !valid {
            self.error_at(
                "InvalidContractExpression",
                "expression is outside the pure, total contract predicate subset",
                expression,
            );
        }
    }

    fn builtin_is_imported(&self, qualified: &str) -> bool {
        let module = self.analyzer.program.definitions[self.environment.owner].module;
        self.analyzer.program.modules[module]
            .imports
            .iter()
            .any(|import| import.path.as_string() == qualified)
    }

    fn flow_state(&self) -> FlowState {
        FlowState {
            self_dirty: self.self_dirty,
            borrows: self.borrows.clone(),
        }
    }

    fn expression_diverges(&self, expression: ExprId) -> bool {
        self.semantics
            .expression_types
            .get(expression)
            .is_some_and(|ty| self.analyzer.typed.types.data(*ty) == &TyData::Never)
            || matches!(
                self.semantics.expression_coercions.get(expression),
                Some(Coercion::NeverToAny)
            )
    }

    fn restore_flow(&mut self, state: &FlowState) {
        self.self_dirty = state.self_dirty;
        self.borrows.clone_from(&state.borrows);
    }

    fn join_flow_states(&mut self, states: impl IntoIterator<Item = FlowState>) {
        let mut dirty = false;
        let mut borrows = BTreeMap::new();
        for state in states {
            dirty |= state.self_dirty;
            for borrow in state.borrows {
                borrows.entry(borrow.token).or_insert(borrow);
            }
        }
        self.self_dirty = dirty;
        self.borrows = borrows.into_values().collect();
    }

    fn register_borrow(
        &mut self,
        owner: Place,
        mutable: bool,
        region: RegionId,
        token: ViewTokenId,
        span: Span,
        expression: ExprId,
    ) {
        if let Some(conflict) = self
            .borrows
            .iter()
            .find(|borrow| places_overlap(&borrow.owner, &owner) && (mutable || borrow.mutable))
            .cloned()
        {
            self.analyzer.diagnostics.push(
                Diagnostic::error(
                    if conflict.mutable {
                        "BorrowConflict"
                    } else {
                        "ReadonlyBorrowConflict"
                    },
                    "interface argument access conflicts with an active call-scoped access",
                    self.expr_span(expression),
                )
                .with_label(conflict.span, "active interface access begins here"),
            );
            return;
        }
        self.borrows.push(ActiveBorrow {
            owner,
            mutable,
            region,
            token,
            span,
        });
    }

    fn check_borrowed_place_use(&mut self, place: &Place, access: PlaceAccess, expression: ExprId) {
        let Some(conflict) = self
            .borrows
            .iter()
            .find(|borrow| {
                places_overlap(&borrow.owner, place)
                    && (borrow.mutable || access == PlaceAccess::Write)
            })
            .cloned()
        else {
            return;
        };
        self.analyzer.diagnostics.push(
            Diagnostic::error(
                if conflict.mutable {
                    "BorrowConflict"
                } else {
                    "ReadonlyBorrowConflict"
                },
                "argument use conflicts with an active interface call",
                self.expr_span(expression),
            )
            .with_label(conflict.span, "active interface access begins here"),
        );
    }

    fn finish_call_arguments(&mut self, arguments: &[ExprId]) {
        for argument in arguments {
            if let Some(view) = self.semantics.views.get(*argument) {
                let token = view.token;
                self.borrows.retain(|borrow| borrow.token != token);
            }
        }
    }
}

#[derive(Default)]
struct PatternCoverage {
    catch_all: bool,
    bool_values: BTreeSet<bool>,
    variants: BTreeSet<PatternVariant>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum PatternVariant {
    None,
    Some,
    Ok,
    Err,
    ParseFloatInvalidSyntax,
    ParseFloatOutOfRange,
    ParseIntInvalidSyntax,
    ParseIntOutOfRange,
    TaskCompleted,
    TaskFaulted,
    TaskCancelled,
    User(DefId),
}

fn pattern_variant_resolution(variant: PatternVariant) -> Resolution {
    match variant {
        PatternVariant::None => Resolution::Builtin(BuiltinValue::None),
        PatternVariant::Some => Resolution::Builtin(BuiltinValue::Some),
        PatternVariant::Ok => Resolution::Builtin(BuiltinValue::Ok),
        PatternVariant::Err => Resolution::Builtin(BuiltinValue::Err),
        PatternVariant::ParseFloatInvalidSyntax => {
            Resolution::Builtin(BuiltinValue::ParseFloatInvalidSyntax)
        }
        PatternVariant::ParseFloatOutOfRange => {
            Resolution::Builtin(BuiltinValue::ParseFloatOutOfRange)
        }
        PatternVariant::ParseIntInvalidSyntax => {
            Resolution::Builtin(BuiltinValue::ParseIntInvalidSyntax)
        }
        PatternVariant::ParseIntOutOfRange => Resolution::Builtin(BuiltinValue::ParseIntOutOfRange),
        PatternVariant::TaskCompleted => Resolution::Builtin(BuiltinValue::TaskCompleted),
        PatternVariant::TaskFaulted => Resolution::Builtin(BuiltinValue::TaskFaulted),
        PatternVariant::TaskCancelled => Resolution::Builtin(BuiltinValue::TaskCancelled),
        PatternVariant::User(definition) => Resolution::Definition(definition),
    }
}

fn contains_unbound_param(
    types: &crate::TyInterner,
    ty: TyId,
    substitution: &Substitution,
) -> bool {
    match types.data(ty) {
        TyData::Param(parameter) => substitution.get(*parameter).is_none(),
        TyData::Tuple(elements) => elements
            .iter()
            .any(|element| contains_unbound_param(types, *element, substitution)),
        TyData::Option(element) => contains_unbound_param(types, *element, substitution),
        TyData::Result { ok, error } => {
            contains_unbound_param(types, *ok, substitution)
                || contains_unbound_param(types, *error, substitution)
        }
        TyData::Task(output) | TyData::List(output) | TyData::TaskOutcome(output) => {
            contains_unbound_param(types, *output, substitution)
        }
        TyData::Nominal { arguments, .. } => arguments
            .iter()
            .any(|argument| contains_unbound_param(types, *argument, substitution)),
        TyData::Projection { self_ty, .. } => contains_unbound_param(types, *self_ty, substitution),
        TyData::DynTarget(instance) => instance
            .bindings
            .iter()
            .any(|binding| contains_unbound_param(types, binding.ty, substitution)),
        TyData::View { target, .. } => contains_unbound_param(types, *target, substitution),
        TyData::Error | TyData::Never | TyData::Builtin(_) | TyData::SelfType(_) => false,
    }
}

fn unify_type(
    types: &crate::TyInterner,
    pattern: TyId,
    actual: TyId,
    substitution: &mut Substitution,
) -> bool {
    match (types.data(pattern), types.data(actual)) {
        (TyData::Error | TyData::Never, _) | (_, TyData::Error) => true,
        (TyData::Param(parameter), _) => {
            if let Some(previous) = substitution.get(*parameter) {
                previous == actual
            } else {
                substitution.insert(*parameter, actual);
                true
            }
        }
        (TyData::Tuple(left), TyData::Tuple(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| unify_type(types, *left, *right, substitution))
        }
        (TyData::Option(left), TyData::Option(right)) => {
            unify_type(types, *left, *right, substitution)
        }
        (
            TyData::Result {
                ok: left_ok,
                error: left_error,
            },
            TyData::Result {
                ok: right_ok,
                error: right_error,
            },
        ) => {
            unify_type(types, *left_ok, *right_ok, substitution)
                && unify_type(types, *left_error, *right_error, substitution)
        }
        (TyData::Task(left), TyData::Task(right)) => unify_type(types, *left, *right, substitution),
        (
            TyData::Nominal {
                definition: left,
                arguments: left_arguments,
            },
            TyData::Nominal {
                definition: right,
                arguments: right_arguments,
            },
        ) => {
            left == right
                && left_arguments.len() == right_arguments.len()
                && left_arguments
                    .iter()
                    .zip(right_arguments)
                    .all(|(left, right)| unify_type(types, *left, *right, substitution))
        }
        (
            TyData::View {
                mutability: left_mutability,
                target: left_target,
            },
            TyData::View {
                mutability: right_mutability,
                target: right_target,
            },
        ) => {
            left_mutability == right_mutability
                && unify_type(types, *left_target, *right_target, substitution)
        }
        (left, right) => left == right,
    }
}

fn substitution_for(parameters: &[GenericParamId], arguments: &[TyId]) -> Substitution {
    let mut substitution = Substitution::default();
    for (parameter, argument) in parameters.iter().zip(arguments) {
        substitution.insert(*parameter, *argument);
    }
    substitution
}

fn type_parameters_in(types: &crate::TyInterner, ty: TyId) -> BTreeSet<GenericParamId> {
    let mut parameters = BTreeSet::new();
    collect_type_parameters(types, ty, &mut parameters);
    parameters
}

fn collect_type_parameters(
    types: &crate::TyInterner,
    ty: TyId,
    output: &mut BTreeSet<GenericParamId>,
) {
    match types.data(ty) {
        TyData::Param(parameter) => {
            output.insert(*parameter);
        }
        TyData::Tuple(elements) => {
            for element in elements {
                collect_type_parameters(types, *element, output);
            }
        }
        TyData::Option(element) => collect_type_parameters(types, *element, output),
        TyData::Result { ok, error } => {
            collect_type_parameters(types, *ok, output);
            collect_type_parameters(types, *error, output);
        }
        TyData::Task(task_output)
        | TyData::List(task_output)
        | TyData::TaskOutcome(task_output) => {
            collect_type_parameters(types, *task_output, output);
        }
        TyData::Nominal { arguments, .. } => {
            for argument in arguments {
                collect_type_parameters(types, *argument, output);
            }
        }
        TyData::Projection { self_ty, .. } => collect_type_parameters(types, *self_ty, output),
        TyData::DynTarget(instance) => {
            for binding in &instance.bindings {
                collect_type_parameters(types, binding.ty, output);
            }
        }
        TyData::View { target, .. } => collect_type_parameters(types, *target, output),
        TyData::Error | TyData::Never | TyData::Builtin(_) | TyData::SelfType(_) => {}
    }
}

fn is_strict_structural_subterm(types: &crate::TyInterner, candidate: TyId, root: TyId) -> bool {
    match types.data(root) {
        TyData::Tuple(elements) => elements.iter().any(|element| {
            *element == candidate || is_strict_structural_subterm(types, candidate, *element)
        }),
        TyData::Option(element) => {
            *element == candidate || is_strict_structural_subterm(types, candidate, *element)
        }
        TyData::Result { ok, error } => {
            *ok == candidate
                || *error == candidate
                || is_strict_structural_subterm(types, candidate, *ok)
                || is_strict_structural_subterm(types, candidate, *error)
        }
        TyData::Nominal { arguments, .. } => arguments.iter().any(|argument| {
            *argument == candidate || is_strict_structural_subterm(types, candidate, *argument)
        }),
        _ => false,
    }
}

fn places_overlap(left: &Place, right: &Place) -> bool {
    if left.root != right.root {
        return false;
    }
    let shared = left.projections.len().min(right.projections.len());
    left.projections[..shared] == right.projections[..shared]
}

fn builtin_type(name: &str) -> Option<BuiltinType> {
    match name {
        "Bool" => Some(BuiltinType::Bool),
        "Int" => Some(BuiltinType::Int),
        "Float" => Some(BuiltinType::Float),
        "Text" => Some(BuiltinType::Text),
        "Unit" => Some(BuiltinType::Unit),
        "Violation" => Some(BuiltinType::Violation),
        "ContractFault" => Some(BuiltinType::ContractFault),
        "TaskFault" => Some(BuiltinType::TaskFault),
        "Duration" => Some(BuiltinType::Duration),
        "File" => Some(BuiltinType::File),
        "Socket" => Some(BuiltinType::Socket),
        _ => None,
    }
}

fn sort_diagnostics(diagnostics: &mut [Diagnostic]) {
    diagnostics.sort_by(|left, right| {
        left.primary
            .file
            .cmp(&right.primary.file)
            .then(left.primary.range.start.cmp(&right.primary.range.start))
            .then(left.primary.range.end.cmp(&right.primary.range.end))
            .then(left.code.cmp(&right.code))
            .then(left.message.cmp(&right.message))
    });
}

#[cfg(test)]
mod tests {
    use loom_core::FileId;
    use loom_hir::{Expr, Program, SourceUnit, lower_files};
    use loom_syntax::parse_with_file;

    use super::{Analysis, analyze};
    use crate::{CallTarget, Signature, TyData, ViewSource, WitnessSelection, WitnessSource};

    const DYNAMIC_SOURCE_FIXTURE: &str = r"
module sample

pub record Counter {
    value Int
}

pub dyn concept Source {
    associated type Item
    method next(mut self) Option[Self.Item]
}

impl Source for Counter {
    associated type Item = Int

    method next(mut self) Option[Int] {
        self.value = self.value + 1
        Some(self.value)
    }
}

fn consume(source Source[Item = Int]) {
    let ignored = source.next()
    Unit
}
";

    fn analyze_source(source: &str) -> (Program, Analysis) {
        let parsed = parse_with_file(FileId(0), source);
        assert!(
            parsed.diagnostics().is_empty(),
            "parse diagnostics: {:#?}",
            parsed.diagnostics()
        );
        let lowered = lower_files([SourceUnit {
            file: FileId(0),
            syntax: parsed.ast(),
        }]);
        assert!(
            lowered.diagnostics.is_empty(),
            "lowering diagnostics: {:#?}",
            lowered.diagnostics
        );
        let analysis = analyze(&lowered.program);
        (lowered.program, analysis)
    }

    #[test]
    fn resolves_generic_data_and_callable_signatures_definition_first() {
        let parsed = parse_with_file(
            FileId(0),
            "module sample\n\nrecord Boxed[T] {\n    value T\n}\n\nfn wrap[T](value T) Boxed[T] {\n    Boxed { value = value }\n}\n",
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
        let analysis = analyze(&lowered.program);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let callable = analysis
            .typed
            .signatures
            .iter()
            .find_map(|(_, signature)| match signature {
                Signature::Callable(callable) => Some(callable),
                _ => None,
            })
            .unwrap();
        assert!(matches!(
            analysis.typed.types.data(callable.return_ty),
            TyData::Nominal { arguments, .. } if arguments.len() == 1
        ));
    }

    #[test]
    fn test_signature_accepts_unit_and_rejects_parameters() {
        let parsed = parse_with_file(
            FileId(0),
            "module sample\n\ntest fn ok() { Unit }\n\ntest fn bad(value Int) { Unit }\n",
        );
        let lowered = lower_files([SourceUnit {
            file: FileId(0),
            syntax: parsed.ast(),
        }]);
        let analysis = analyze(&lowered.program);
        assert!(
            analysis
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "InvalidTestSignature")
        );
    }

    #[test]
    fn int_min_has_one_legal_literal_spelling() {
        let (_, valid) =
            analyze_source("module sample\n\npub fn minimum() Int { -9223372036854775808 }\n");
        assert!(valid.diagnostics.is_empty(), "{:#?}", valid.diagnostics);

        let (_, invalid) =
            analyze_source("module sample\n\npub fn too_large() Int { 9223372036854775808 }\n");
        assert!(
            invalid
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "IntegerLiteralOutOfRange")
        );
    }

    #[test]
    fn core01_price_order_source_reaches_body_checker() {
        let parsed = parse_with_file(
            FileId(0),
            include_str!("../../../examples/core01/shop.loom"),
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
        let analysis = analyze(&lowered.program);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:#?}",
            analysis.diagnostics
        );
    }

    #[test]
    fn core02_concept_source_reaches_body_checker() {
        let parsed = parse_with_file(
            FileId(0),
            include_str!("../../../examples/core02/concepts.loom"),
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
        let analysis = analyze(&lowered.program);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:#?}",
            analysis.diagnostics
        );
        assert_eq!(analysis.typed.conformances.iter().count(), 3);

        let calls = analysis
            .typed
            .bodies
            .iter()
            .flat_map(|(_, body)| body.calls.values())
            .collect::<Vec<_>>();
        assert!(
            calls
                .iter()
                .any(|call| matches!(call.target, CallTarget::DynamicConcept { .. }))
        );

        let views = analysis
            .typed
            .bodies
            .iter()
            .flat_map(|(_, body)| body.views.values())
            .collect::<Vec<_>>();
        assert!(views.len() >= 6);
        assert!(views.iter().any(|view| {
            matches!(
                &view.source,
                ViewSource::Concrete {
                    witness: WitnessSelection {
                        source: WitnessSource::Implementation(_),
                        ..
                    },
                    ..
                }
            )
        }));
        assert!(
            views
                .iter()
                .any(|view| matches!(view.source, ViewSource::Interface { .. }))
        );
        assert!(
            analysis
                .typed
                .bodies
                .iter()
                .all(|(_, body)| body.view_moves.is_empty())
        );
    }

    #[test]
    fn dynamic_arguments_are_implicit_and_call_scoped() {
        let source = format!(
            "{DYNAMIC_SOURCE_FIXTURE}\n\
             test fn call_dynamic() {{\n\
                 var counter = Counter {{ value = 0 }}\n\
                 consume(counter)\n\
                 let observed = counter.value\n\
                 assert observed == 1\n\
                 Unit\n\
             }}\n"
        );
        let (_, analysis) = analyze_source(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:#?}",
            analysis.diagnostics
        );
        assert_eq!(
            analysis
                .typed
                .bodies
                .iter()
                .flat_map(|(_, body)| body.views.values())
                .count(),
            1
        );
        assert!(
            analysis
                .typed
                .bodies
                .iter()
                .all(|(_, body)| body.view_moves.is_empty())
        );
    }

    #[test]
    fn mutating_dynamic_concept_requires_a_variable_argument() {
        let source = format!(
            "{DYNAMIC_SOURCE_FIXTURE}\n\
             test fn immutable_argument() {{\n\
                 let counter = Counter {{ value = 0 }}\n\
                 consume(counter)\n\
                 Unit\n\
             }}\n"
        );
        let (_, analysis) = analyze_source(&source);
        assert!(
            analysis
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "DynMutReceiverUnavailable")
        );
    }

    #[test]
    fn invariant_isolation_forks_branches_and_joins_dirty_state() {
        let (_, analysis) = analyze_source(
            r"
module sample

pub record Pair {
    value Int
    invariant self.value >= 0
}

impl Pair {
    method observe(self) {
        Unit
    }

    method mutate(mut self) {
        if true {
            self.value = 1
        } else {
            self.observe()
        }
        self.observe()
        Unit
    }
}
",
        );
        let isolation = analysis
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "InvariantIsolationViolation")
            .count();
        assert_eq!(isolation, 1, "{:#?}", analysis.diagnostics);
    }

    #[test]
    fn nested_variant_binding_does_not_make_match_exhaustive() {
        let (_, analysis) = analyze_source(
            r"
module sample

fn unwrap_some(value Option[Int]) Int {
    match value {
        Some(inner) => inner
    }
}
",
        );
        assert!(
            analysis
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "NonExhaustiveMatch")
        );
    }

    #[test]
    fn record_side_table_preserves_source_evaluation_and_canonical_layout() {
        let (program, analysis) = analyze_source(
            r"
module sample

pub record Pair {
    first Int
    second Int
}

fn first_value() Int { 1 }
fn second_value() Int { 2 }

fn make_pair() Pair {
    Pair {
        second = second_value()
        first = first_value()
    }
}
",
        );
        assert!(
            analysis.diagnostics.is_empty(),
            "{:#?}",
            analysis.diagnostics
        );
        let (body_id, expression, source_names) = program
            .bodies
            .iter()
            .find_map(|(body_id, body)| {
                body.expressions.iter().find_map(|(expression, node)| {
                    let Expr::RecordLiteral { fields, .. } = node else {
                        return None;
                    };
                    Some((
                        body_id,
                        expression,
                        fields
                            .iter()
                            .map(|field| field.name.as_str().to_owned())
                            .collect::<Vec<_>>(),
                    ))
                })
            })
            .expect("record literal");
        assert_eq!(source_names, ["second", "first"]);
        let canonical = analysis
            .typed
            .bodies
            .get(body_id)
            .expect("body semantics")
            .record_fields
            .get(expression)
            .expect("canonical record map");
        let canonical_names = canonical
            .iter()
            .map(|(field, _)| {
                program.definitions[*field]
                    .name
                    .as_ref()
                    .expect("field name")
                    .as_str()
            })
            .collect::<Vec<_>>();
        assert_eq!(canonical_names, ["first", "second"]);
    }

    #[test]
    fn unique_bound_resolves_unqualified_associated_projection() {
        let (_, analysis) = analyze_source(
            r"
module sample

pub concept Source {
    associated type Item
    method read(self) Self.Item
}

pub record Number {
    value Int
}

impl Source for Number {
    associated type Item = Int

    method read(self) Int {
        self.value
    }
}

fn read[T: Source](source T) T.Item {
    source.read()
}
",
        );
        assert!(
            analysis.diagnostics.is_empty(),
            "{:#?}",
            analysis.diagnostics
        );
        assert!(
            analysis
                .typed
                .resolved_type_refs
                .values()
                .any(|ty| matches!(analysis.typed.types.data(*ty), TyData::Projection { .. }))
        );
    }

    #[test]
    fn concrete_call_normalizes_associated_projection_from_selected_witness() {
        let (_, analysis) = analyze_source(
            r"
module sample

pub concept Source {
    associated type Item
    method read(self) Self.Item
}

pub record Number {
    value Int
}

impl Source for Number {
    associated type Item = Int

    method read(self) Int {
        self.value
    }
}

fn read[T: Source](source T) T.Item {
    source.read()
}

test fn concrete_projection() {
    let value = read(Number { value = 3 })
    assert value == 3
    Unit
}
",
        );
        assert!(
            analysis.diagnostics.is_empty(),
            "{:#?}",
            analysis.diagnostics
        );
    }

    #[test]
    fn never_branch_joins_to_the_other_if_type() {
        let (_, analysis) = analyze_source(
            r"
module sample

fn choose(flag Bool) Int {
    if flag {
        return 0
    } else {
        1
    }
}
",
        );
        assert!(
            analysis.diagnostics.is_empty(),
            "{:#?}",
            analysis.diagnostics
        );
    }

    #[test]
    fn parse_float_error_variants_are_matchable_and_exhaustive() {
        let (_, analysis) = analyze_source(
            r"
module sample

import standard.float.parse_float

fn classify(text Text) Int {
    match parse_float(text) {
        Ok(_) => 0
        Err(error) => match error {
            ParseFloatError.InvalidSyntax => 1
            ParseFloatError.OutOfRange => 2
        }
    }
}
",
        );
        assert!(
            analysis.diagnostics.is_empty(),
            "{:#?}",
            analysis.diagnostics
        );
    }

    #[test]
    fn concept_owner_can_implement_a_concrete_primitive_head() {
        let (_, analysis) = analyze_source(
            r"
module sample

pub concept Zero {
    static method zero() Self
}

impl Zero for Int {
    static method zero() Int {
        0
    }
}

fn make_zero() Int {
    <Int as Zero>.zero()
}
",
        );
        assert!(
            analysis.diagnostics.is_empty(),
            "{:#?}",
            analysis.diagnostics
        );
    }
}
