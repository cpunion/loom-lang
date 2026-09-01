//! Whole-program semantic analysis entry point.

use std::collections::{BTreeMap, BTreeSet};

use loom_core::{Diagnostic, LOOM_LANGUAGE_VERSION, Name, PackageId, Severity, Span};
use loom_hir::{
    BinaryOp, BodyId, BodyKind, DefId, DefinitionKind, Expr, ExprId, GenericParamId, Literal,
    LocalId, MatchArm, ModuleId, ParamId, Path, Pattern, PatternId, Program, ReceiverKind,
    Statement, TypeArgumentRef, TypeRef, TypeRefId, UnaryOp, Visibility,
};

use crate::proof::{
    ProofBinary, ProofFacts, ProofPlace, ProofResult, ProofRoot, ProofTerm, ProofUnary,
};
use crate::{
    AssociatedTypeBinding, BodySemantics, Bound, BuiltinType, BuiltinValue, CallResolution,
    CallTarget, CallableSignature, Coercion, ConceptInstance, ConstantValue, ConstructionCheck,
    DefMapBuild, Goal, ImplHeader, ImplScope, ModuleGraph, ModuleGraphBuild, Mutability, Namespace,
    ParamEnv, Place, PlaceProjection, PlaceRoot, ReceiverPassing, RegionId, Resolution,
    ResolveError, RuntimeCheck, ScopedDisposal, Signature, SolveFailure, Substitution,
    TaskIntrinsic, TyData, TyId, TypedProgram, ViewResolution, ViewSource, ViewTokenId,
    WitnessSelection, WitnessSource,
};

const RESOURCE_MODULE: &str = "std.resource";
const FLOAT_MODULE: &str = "std.float";
const FILE_MODULE: &str = "std.file";
const IO_MODULE: &str = "std.io";
const JSON_MODULE: &str = "std.json";
const LOG_MODULE: &str = "std.log";
const NET_MODULE: &str = "std.net";
const PATH_MODULE: &str = "std.path";
const TEXT_MODULE: &str = "std.text";
const DISPOSE_CONCEPT: &str = "Dispose";
const MUST_SCOPE_CONCEPT: &str = "MustScope";
const NO_SUSPEND_CONCEPT: &str = "NoSuspend";
const CONSTANT_EVALUATION_DEPTH_LIMIT: usize = 256;
const CONSTANT_EVALUATION_WORK_LIMIT: usize = 65_536;

/// Complete checker output. A typed program remains available after errors for
/// diagnostics and editor features, but only an error-free analysis is
/// executable.
#[derive(Clone, Debug)]
pub struct Analysis {
    pub typed: TypedProgram,
    pub module_graph: ModuleGraph,
    pub def_maps: DefMapBuild,
    pub impl_index: crate::ImplIndex,
    /// Compiler-known concept identities resolved from their module-qualified
    /// HIR definitions. Downstream lowering must not reconstruct these facts
    /// from an unqualified source name.
    pub canonical_concepts: CanonicalConcepts,
    /// Compiler-owned standard-library declarations whose exact source
    /// identity affects language lowering or proof recognition.
    pub canonical_std_items: CanonicalStdItems,
    pub diagnostics: Vec<Diagnostic>,
}

/// Canonical language concepts whose identity affects executable validation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CanonicalConcepts {
    /// The `std.resource.Dispose` cleanup concept selected by semantic analysis.
    pub dispose: Option<DefId>,
    /// The canonical `Dispose.dispose(mut self)` requirement.
    pub dispose_requirement: Option<DefId>,
    /// The `std.resource.MustScope` marker selected by semantic analysis.
    pub must_scope: Option<DefId>,
    /// The `std.resource.NoSuspend` marker selected by semantic analysis.
    pub no_suspend: Option<DefId>,
}

/// Exact compiler-owned standard-library source declarations recognized by
/// semantic analysis. Public calls still resolve and execute as ordinary
/// source definitions; these identities grant no builtin-call authority.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CanonicalStdItems {
    pub is_finite: Option<DefId>,
    pub decode_text_error: Option<DefId>,
    pub file: Option<DefId>,
    pub io_error: Option<DefId>,
    pub io_error_kind: Option<DefId>,
    pub json: Option<DefId>,
    pub json_error: Option<DefId>,
    pub log_level: Option<DefId>,
    pub path_error: Option<DefId>,
    pub socket: Option<DefId>,
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
///
/// This is a trusted compiler-embedding boundary over already-lowered HIR. The
/// caller must have validated package resolution and source ownership before
/// constructing [`Program`]; semantic analysis treats each package id as
/// nominal identity and does not authenticate untrusted package input.
#[must_use]
pub fn analyze(program: &Program) -> Analysis {
    analyze_with_reused_bodies(program, None)
}

/// Rebuilds declarations and checks only bodies absent from `reusable`.
///
/// The caller must provide a previous error-free analysis of the same HIR
/// declaration shape. Keeping the previous type interner preserves every
/// cached [`TyId`] while changed bodies may append newly inferred types.
#[must_use]
pub fn analyze_reusing_bodies(
    program: &Program,
    previous: &Analysis,
    reusable: &BTreeSet<BodyId>,
) -> Analysis {
    analyze_with_reused_bodies(program, Some((previous, reusable)))
}

fn analyze_with_reused_bodies(
    program: &Program,
    previous: Option<(&Analysis, &BTreeSet<BodyId>)>,
) -> Analysis {
    let graph = ModuleGraphBuild::build(program);
    let def_maps = DefMapBuild::build(program);
    let mut diagnostics = graph.diagnostics;
    diagnostics.extend(def_maps.diagnostics.iter().cloned());

    let (typed, impl_index, canonical_concepts, canonical_std_items, mut diagnostics) = {
        let mut typed = TypedProgram::default();
        if let Some((previous, _)) = previous {
            typed.types = previous.typed.types.clone();
        }
        let mut analyzer = Analyzer {
            program,
            def_maps: &def_maps,
            typed,
            impl_index: crate::ImplIndex::default(),
            canonical_concepts: CanonicalConcepts::default(),
            canonical_std_items: CanonicalStdItems::default(),
            diagnostics,
        };
        let canonical_std_items = analyzer.resolve_canonical_std_items();
        analyzer.canonical_std_items = canonical_std_items;
        analyzer.collect_signatures();
        analyzer.validate_recursive_value_types();
        analyzer.validate_dynamic_concepts();
        analyzer.build_conformances();
        let canonical_concepts = analyzer.resolve_canonical_concepts();
        analyzer.canonical_concepts = canonical_concepts;
        analyzer.validate_resource_concepts(canonical_concepts);
        analyzer.check_bodies(previous);
        (
            analyzer.typed,
            analyzer.impl_index,
            canonical_concepts,
            canonical_std_items,
            analyzer.diagnostics,
        )
    };
    sort_diagnostics(&mut diagnostics);

    Analysis {
        typed,
        module_graph: graph.graph,
        def_maps,
        impl_index,
        canonical_concepts,
        canonical_std_items,
        diagnostics,
    }
}

struct Analyzer<'a> {
    program: &'a Program,
    def_maps: &'a DefMapBuild,
    typed: TypedProgram,
    impl_index: crate::ImplIndex,
    canonical_concepts: CanonicalConcepts,
    canonical_std_items: CanonicalStdItems,
    diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConstantEvaluationState {
    Evaluating,
    Complete,
    Failed,
}

#[derive(Clone, Copy)]
enum ConstantLimitFrame {
    Expression {
        body: BodyId,
        expression: ExprId,
        depth: usize,
    },
    Complete(DefId),
}

#[derive(Clone, Debug)]
struct TypeContext {
    module: ModuleId,
    generic_params: BTreeMap<Name, GenericParamId>,
    self_ty: Option<TyId>,
    self_concept: Option<DefId>,
}

impl Analyzer<'_> {
    fn canonical_prelude_type(&self, path: &Path) -> Option<DefId> {
        let [segment] = path.segments.as_slice() else {
            return None;
        };
        match segment.name.as_str() {
            "File" => self.canonical_std_items.file,
            "IoError" => self.canonical_std_items.io_error,
            "IoErrorKind" => self.canonical_std_items.io_error_kind,
            "Json" => self.canonical_std_items.json,
            "JsonError" => self.canonical_std_items.json_error,
            "Socket" => self.canonical_std_items.socket,
            _ => None,
        }
    }

    fn resolve_language_concept(&self, name: &str) -> Option<DefId> {
        let std = PackageId::compiler_std(LOOM_LANGUAGE_VERSION);
        self.program
            .definitions
            .iter()
            .find_map(|(definition, item)| {
                let module = &self.program.modules[item.module];
                (module.package == std
                    && module.name.as_str() == RESOURCE_MODULE
                    && item
                        .name
                        .as_ref()
                        .is_some_and(|candidate| candidate.as_str() == name)
                    && matches!(item.kind, DefinitionKind::Concept(_)))
                .then_some(definition)
            })
    }

    fn resolve_canonical_concepts(&self) -> CanonicalConcepts {
        let dispose = self.resolve_language_concept(DISPOSE_CONCEPT);
        let dispose_requirement = dispose.and_then(|definition| {
            let DefinitionKind::Concept(concept) = &self.program.definitions[definition].kind
            else {
                return None;
            };
            concept.requirements.iter().copied().find(|requirement| {
                self.program.definitions[*requirement]
                    .name
                    .as_ref()
                    .is_some_and(|name| name.as_str() == "dispose")
            })
        });
        CanonicalConcepts {
            dispose,
            dispose_requirement,
            must_scope: self.resolve_language_concept(MUST_SCOPE_CONCEPT),
            no_suspend: self.resolve_language_concept(NO_SUSPEND_CONCEPT),
        }
    }

    fn resolve_compiler_std_definition(
        &self,
        module_name: &str,
        item_name: &str,
        kind: fn(&DefinitionKind) -> bool,
    ) -> Option<DefId> {
        let std = PackageId::compiler_std(LOOM_LANGUAGE_VERSION);
        self.program
            .definitions
            .iter()
            .find_map(|(definition, item)| {
                let module = &self.program.modules[item.module];
                (module.package == std
                    && module.name.as_str() == module_name
                    && item
                        .name
                        .as_ref()
                        .is_some_and(|candidate| candidate.as_str() == item_name)
                    && kind(&item.kind))
                .then_some(definition)
            })
    }

    fn resolve_canonical_std_items(&self) -> CanonicalStdItems {
        CanonicalStdItems {
            is_finite: self.resolve_compiler_std_definition(FLOAT_MODULE, "is_finite", |kind| {
                matches!(kind, DefinitionKind::Function(_))
            }),
            decode_text_error: self.resolve_compiler_std_definition(
                TEXT_MODULE,
                "DecodeTextError",
                |kind| matches!(kind, DefinitionKind::Enum(_)),
            ),
            file: self.resolve_compiler_std_definition(FILE_MODULE, "File", |kind| {
                matches!(kind, DefinitionKind::Record(_))
            }),
            io_error: self.resolve_compiler_std_definition(IO_MODULE, "IoError", |kind| {
                matches!(kind, DefinitionKind::Record(_))
            }),
            io_error_kind: self.resolve_compiler_std_definition(IO_MODULE, "IoErrorKind", |kind| {
                matches!(kind, DefinitionKind::Enum(_))
            }),
            json: self.resolve_compiler_std_definition(JSON_MODULE, "Json", |kind| {
                matches!(kind, DefinitionKind::Enum(_))
            }),
            json_error: self.resolve_compiler_std_definition(JSON_MODULE, "JsonError", |kind| {
                matches!(kind, DefinitionKind::Enum(_))
            }),
            log_level: self.resolve_compiler_std_definition(LOG_MODULE, "LogLevel", |kind| {
                matches!(kind, DefinitionKind::Enum(_))
            }),
            path_error: self.resolve_compiler_std_definition(PATH_MODULE, "PathError", |kind| {
                matches!(kind, DefinitionKind::Enum(_))
            }),
            socket: self.resolve_compiler_std_definition(NET_MODULE, "Socket", |kind| {
                matches!(kind, DefinitionKind::Record(_))
            }),
        }
    }

    fn validate_resource_concepts(&mut self, canonical: CanonicalConcepts) {
        for (marker, definition) in [
            (MUST_SCOPE_CONCEPT, canonical.must_scope),
            (NO_SUSPEND_CONCEPT, canonical.no_suspend),
        ] {
            let Some(definition) = definition else {
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
                    format!("std.resource.{marker} must be an empty, non-dyn marker concept"),
                    self.definition_span(definition),
                );
            }
        }

        let Some(definition) = canonical.dispose else {
            return;
        };
        let DefinitionKind::Concept(concept) = &self.program.definitions[definition].kind else {
            return;
        };
        let valid_header = !concept.dyn_capable
            && concept.associated_types.is_empty()
            && concept.requirements.len() == 1;
        let valid_method = canonical.dispose_requirement.is_some_and(|requirement| {
            if concept.requirements.as_slice() != [requirement] {
                return false;
            }
            let name_ok = self.program.definitions[requirement]
                .name
                .as_ref()
                .is_some_and(|name| name.as_str() == "dispose");
            let source_ok = match &self.program.definitions[requirement].kind {
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
                self.typed.signatures.get(requirement),
                Some(Signature::Callable(signature))
                    if !signature.is_async
                        && signature.return_ty == self.typed.types.builtin(BuiltinType::Unit)
            );
            name_ok && source_ok && signature_ok
        });
        if !valid_header || !valid_method {
            self.error(
                "InvalidDisposeConcept",
                "std.resource.Dispose must be a non-dyn concept containing only `method dispose(mut self)` without contracts",
                self.definition_span(definition),
            );
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
                | DefinitionKind::Constant(_)
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
                DefinitionKind::Constant(constant) => {
                    let ty = self.resolve_type_ref(constant.ty, &context);
                    if !matches!(
                        self.typed.types.data(ty),
                        TyData::Builtin(
                            BuiltinType::Bool
                                | BuiltinType::Int
                                | BuiltinType::Float
                                | BuiltinType::Text
                        )
                    ) {
                        self.error(
                            "InvalidConstantType",
                            "a constant type must be Bool, Int, Float, or Text",
                            self.type_span(constant.ty),
                        );
                    }
                    self.typed
                        .signatures
                        .insert(definition, Signature::Constant { ty });
                }
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

    /// Rejects nominal declarations whose by-value representation would have
    /// infinite size. This runs only after every declared type has been
    /// resolved, so declaration and source order cannot affect the graph.
    fn validate_recursive_value_types(&mut self) {
        let nominal_definitions = self
            .program
            .definitions
            .iter()
            .filter_map(|(definition, item)| {
                matches!(
                    item.kind,
                    DefinitionKind::RefinedType(_)
                        | DefinitionKind::Record(_)
                        | DefinitionKind::Enum(_)
                )
                .then_some(definition)
            })
            .collect::<BTreeSet<_>>();
        let mut graph = nominal_definitions
            .iter()
            .copied()
            .map(|definition| (definition, Vec::new()))
            .collect::<BTreeMap<_, _>>();

        for definition in &nominal_definitions {
            let roots = match &self.program.definitions[*definition].kind {
                DefinitionKind::RefinedType(refined) => self
                    .typed
                    .resolved_type_refs
                    .get(refined.base)
                    .copied()
                    .into_iter()
                    .collect(),
                DefinitionKind::Record(record) => record
                    .fields
                    .iter()
                    .filter_map(|field| match self.typed.signatures.get(*field) {
                        Some(Signature::Field { ty, .. }) => Some(*ty),
                        _ => None,
                    })
                    .collect(),
                DefinitionKind::Enum(enumeration) => enumeration
                    .variants
                    .iter()
                    .filter_map(|variant| match self.typed.signatures.get(*variant) {
                        Some(Signature::Variant { payload, .. }) => Some(payload.as_slice()),
                        _ => None,
                    })
                    .flatten()
                    .copied()
                    .collect(),
                _ => Vec::new(),
            };
            let mut dependencies = BTreeSet::new();
            for root in roots {
                collect_by_value_nominal_dependencies(
                    &self.typed.types,
                    root,
                    &nominal_definitions,
                    &mut dependencies,
                );
            }
            graph.insert(*definition, dependencies.into_iter().collect());
        }

        for mut component in strongly_connected_nominal_components(&graph) {
            component.sort_unstable();
            let is_cycle = component.len() > 1
                || graph
                    .get(&component[0])
                    .is_some_and(|dependencies| dependencies.contains(&component[0]));
            if !is_cycle {
                continue;
            }

            self.report_recursive_value_component(&component);
        }
    }

    fn report_recursive_value_component(&mut self, component: &[DefId]) {
        let names = component
            .iter()
            .map(|definition| {
                self.program.definitions[*definition]
                    .name
                    .as_ref()
                    .map_or("<anonymous>", Name::as_str)
            })
            .collect::<Vec<_>>();
        let message = if names.len() == 1 {
            format!(
                "by-value nominal type `{}` has infinite size because it contains itself",
                names[0]
            )
        } else {
            format!(
                "by-value nominal types {} form an infinite-size cycle",
                names
                    .iter()
                    .map(|name| format!("`{name}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let mut diagnostic = Diagnostic::error(
            "RecursiveValueType",
            message,
            self.definition_span(component[0]),
        )
        .with_note("break the cycle with an indirect List, TextMap, Task, or dynamic concept view");
        for definition in component.iter().skip(1) {
            diagnostic = diagnostic.with_label(
                self.definition_span(*definition),
                "this declaration participates in the by-value cycle",
            );
        }
        self.diagnostics.push(diagnostic);
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
                "TextMap" => {
                    if type_arguments.len() != 1 {
                        self.report_arity(reference, "TextMap", 1, type_arguments.len());
                        return self.typed.types.error();
                    }
                    return self.typed.types.intern(TyData::TextMap(type_arguments[0]));
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
        let definition = self
            .canonical_prelude_type(constructor)
            .or_else(|| self.resolve_definition(constructor, Namespace::Type, context.module));
        let Some(definition) = definition else {
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
            if let Some(definition) = self.canonical_prelude_type(path) {
                return self.typed.types.intern(TyData::Nominal {
                    definition,
                    arguments: Vec::new(),
                });
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
            DefinitionKind::AssociatedType(associated) => {
                match &self.program.definitions[associated.owner].kind {
                    DefinitionKind::Concept(_) => {
                        Some(self.typed.types.intern(TyData::SelfType(associated.owner)))
                    }
                    DefinitionKind::Conformance(conformance) => {
                        let context = TypeContext {
                            module: self.program.definitions[associated.owner].module,
                            generic_params: generic_params.clone(),
                            self_ty: None,
                            self_concept: None,
                        };
                        Some(self.resolve_type_ref(conformance.target, &context))
                    }
                    _ => None,
                }
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
            DefinitionKind::AssociatedType(associated) => {
                match &self.program.definitions[associated.owner].kind {
                    DefinitionKind::Concept(_) => Some(associated.owner),
                    DefinitionKind::Conformance(conformance) => {
                        let module = self.program.definitions[associated.owner].module;
                        crate::Resolver::new(self.program, self.def_maps, module)
                            .resolve_definition(&conformance.concept.path, Namespace::Concept)
                            .ok()
                    }
                    _ => None,
                }
            }
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
            DefinitionKind::Error
            | DefinitionKind::Constant(_)
            | DefinitionKind::RefinedType(_)
            | DefinitionKind::Concept(_) => {}
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
        self.validate_associated_type_bindings();
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
    fn validate_associated_type_bindings(&mut self) {
        let conformances = self
            .typed
            .conformances
            .iter()
            .map(|(definition, semantics)| (definition, semantics.clone()))
            .collect::<Vec<_>>();
        let projection_bindings = conformances
            .iter()
            .flat_map(|(definition, semantics)| {
                semantics.associated_types.iter().map(|binding| {
                    (
                        (
                            semantics.target,
                            semantics.concept.concept,
                            binding.associated_type,
                        ),
                        (*definition, binding.ty),
                    )
                })
            })
            .collect::<BTreeMap<_, _>>();
        let cyclic = projection_bindings
            .iter()
            .filter_map(|(key, (definition, _))| {
                projection_binding_has_cycle(
                    *key,
                    &projection_bindings,
                    &self.typed.types,
                    &mut BTreeSet::new(),
                    0,
                )
                .then_some(*definition)
            })
            .collect::<BTreeSet<_>>();
        for definition in &cyclic {
            self.error(
                "ConformanceResolutionCycle",
                "associated type bindings form a projection cycle",
                self.definition_span(*definition),
            );
        }

        for (definition, semantics) in conformances {
            if cyclic.contains(&definition) {
                continue;
            }
            let environment = ParamEnv {
                bounds: self
                    .impl_index
                    .header(definition)
                    .map_or_else(Vec::new, |header| header.conditions.clone()),
            };
            for binding in &semantics.associated_types {
                let Some(Signature::AssociatedType { bounds, .. }) =
                    self.typed.signatures.get(binding.associated_type).cloned()
                else {
                    continue;
                };
                for bound in bounds {
                    let self_ty = self.instantiate_concept_type(
                        bound.self_ty,
                        semantics.target,
                        &semantics.concept,
                    );
                    let bindings = bound
                        .concept
                        .bindings
                        .into_iter()
                        .map(|binding| AssociatedTypeBinding {
                            associated_type: binding.associated_type,
                            ty: self.instantiate_concept_type(
                                binding.ty,
                                semantics.target,
                                &semantics.concept,
                            ),
                        })
                        .collect();
                    let goal = Goal {
                        self_ty,
                        concept: ConceptInstance {
                            concept: bound.concept.concept,
                            bindings,
                        },
                    };
                    let result = {
                        let module = self.program.definitions[definition].module;
                        let scope = ImplScope::for_module(self.program, module);
                        let mut solver = crate::ConformanceSolver::new_in_scope(
                            &self.impl_index,
                            &mut self.typed.types,
                            scope,
                        );
                        solver.solve(&goal, &environment)
                    };
                    if let Err(failure) = result {
                        let associated_name = self.program.definitions[binding.associated_type]
                            .name
                            .as_ref()
                            .map_or("<error>", Name::as_str);
                        let bound_name = self.program.definitions[goal.concept.concept]
                            .name
                            .as_ref()
                            .map_or("<error>", Name::as_str);
                        let reason = match failure {
                            SolveFailure::Missing => "no matching conformance exists",
                            SolveFailure::Ambiguous(_) => "more than one conformance matches",
                            SolveFailure::Cycle(_) => "proof search entered a cycle",
                            SolveFailure::AssociatedTypeMismatch { .. } => {
                                "an associated binding does not match"
                            }
                        };
                        self.diagnostics.push(
                            Diagnostic::error(
                                "AssociatedTypeBoundNotSatisfied",
                                format!(
                                    "associated type `{associated_name}` must satisfy `{bound_name}` ({reason})"
                                ),
                                self.definition_span(definition),
                            )
                            .with_label(
                                self.definition_span(binding.associated_type),
                                "bound declared here",
                            ),
                        );
                    }
                }
            }
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
            TyData::Nominal { .. }
                | TyData::Builtin(_)
                | TyData::Option(_)
                | TyData::Result { .. }
                | TyData::TextMap(_)
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
                scope: ImplScope::for_module(
                    self.program,
                    self.program.definitions[definition].module,
                ),
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
            TyData::Tuple(elements) => {
                let elements = elements
                    .into_iter()
                    .map(|element| self.instantiate_concept_type(element, concrete_self, instance))
                    .collect();
                self.typed.types.intern(TyData::Tuple(elements))
            }
            TyData::List(element) => {
                let element = self.instantiate_concept_type(element, concrete_self, instance);
                self.typed.types.intern(TyData::List(element))
            }
            TyData::TextMap(value) => {
                let value = self.instantiate_concept_type(value, concrete_self, instance);
                self.typed.types.intern(TyData::TextMap(value))
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
            TyData::Task(output) => {
                let output = self.instantiate_concept_type(output, concrete_self, instance);
                self.typed.types.intern(TyData::Task(output))
            }
            TyData::TaskOutcome(output) => {
                let output = self.instantiate_concept_type(output, concrete_self, instance);
                self.typed.types.intern(TyData::TaskOutcome(output))
            }
            TyData::DynTarget(target) => {
                let bindings = target
                    .bindings
                    .into_iter()
                    .map(|binding| AssociatedTypeBinding {
                        associated_type: binding.associated_type,
                        ty: self.instantiate_concept_type(binding.ty, concrete_self, instance),
                    })
                    .collect();
                self.typed.types.intern(TyData::DynTarget(ConceptInstance {
                    concept: target.concept,
                    bindings,
                }))
            }
            TyData::View { mutability, target } => {
                let target = self.instantiate_concept_type(target, concrete_self, instance);
                self.typed.types.intern(TyData::View { mutability, target })
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
            TypeRef::Dyn(target) => target
                .bindings
                .iter()
                .any(|binding| self.type_ref_leaks_self(binding.ty, concept)),
            TypeRef::Error | TypeRef::Path(_) => false,
        }
    }

    fn check_bodies(&mut self, previous: Option<(&Analysis, &BTreeSet<BodyId>)>) {
        // Body checking is implemented below in this module; keeping this
        // separate from signature collection guarantees definition-site generic
        // checking and order-independent call resolution.
        let bodies = self
            .program
            .bodies
            .iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        let (constant_bodies, mut bodies): (Vec<_>, Vec<_>) = bodies
            .into_iter()
            .partition(|body| self.program.bodies[*body].kind == BodyKind::Constant);
        for body in constant_bodies {
            self.check_body(body);
        }
        self.evaluate_constants();
        bodies.sort_by_key(|body| {
            let rank = match self.program.bodies[*body].kind {
                BodyKind::RefinementPredicate => 0,
                BodyKind::RecordInvariant => 1,
                BodyKind::Requires => 2,
                BodyKind::Ensures => 3,
                BodyKind::Function | BodyKind::Method => 4,
                BodyKind::Constant => unreachable!("constant bodies were partitioned above"),
            };
            (rank, body.raw())
        });
        for body in bodies {
            if let Some((previous, reusable)) = previous
                && reusable.contains(&body)
                && let Some(semantics) = previous.typed.bodies.get(body)
            {
                self.typed.bodies.insert(body, semantics.clone());
                continue;
            }
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

    fn evaluate_constants(&mut self) {
        let definitions = self
            .program
            .definitions
            .iter()
            .filter_map(|(definition, item)| {
                matches!(item.kind, DefinitionKind::Constant(_)).then_some(definition)
            })
            .collect::<Vec<_>>();
        // Check every root from an empty local traversal. Global evaluation
        // cache state and declaration order therefore cannot make an
        // over-limit initializer appear cheaper than the same source checked
        // in another order.
        let accepted = definitions
            .iter()
            .copied()
            .filter(|definition| self.constant_evaluation_within_limits(*definition))
            .collect::<BTreeSet<_>>();
        let mut states = BTreeMap::new();
        for definition in definitions.iter().copied() {
            if !accepted.contains(&definition) {
                states.insert(definition, ConstantEvaluationState::Failed);
            }
        }
        for definition in definitions {
            self.evaluate_constant(definition, &mut states);
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "keeping the iterative traversal together makes its depth and work accounting auditable"
    )]
    fn constant_evaluation_within_limits(&mut self, definition: DefId) -> bool {
        let DefinitionKind::Constant(constant) = self.program.definitions[definition].kind.clone()
        else {
            return true;
        };
        let root = self.program.bodies[constant.value].root;
        let mut active = BTreeSet::from([definition]);
        let mut max_seen_depth = BTreeMap::from([(definition, 1_usize)]);
        let mut work = 0_usize;
        let mut pending = vec![
            ConstantLimitFrame::Complete(definition),
            ConstantLimitFrame::Expression {
                body: constant.value,
                expression: root,
                depth: 1,
            },
        ];
        while let Some(frame) = pending.pop() {
            let ConstantLimitFrame::Expression {
                body,
                expression,
                depth,
            } = frame
            else {
                let ConstantLimitFrame::Complete(definition) = frame else {
                    unreachable!()
                };
                active.remove(&definition);
                continue;
            };
            if depth > CONSTANT_EVALUATION_DEPTH_LIMIT {
                self.error(
                    "ConstantEvaluationLimit",
                    format!(
                        "constant evaluation exceeds the maximum expression depth of {CONSTANT_EVALUATION_DEPTH_LIMIT}"
                    ),
                    self.definition_span(definition),
                );
                return false;
            }
            work = work.saturating_add(1);
            if work > CONSTANT_EVALUATION_WORK_LIMIT {
                self.error(
                    "ConstantEvaluationLimit",
                    format!(
                        "constant evaluation exceeds the maximum work of {CONSTANT_EVALUATION_WORK_LIMIT} expressions"
                    ),
                    self.definition_span(definition),
                );
                return false;
            }

            let next_depth = depth.saturating_add(1);
            match self.program.bodies[body].expressions[expression].clone() {
                Expr::Path(_) => {
                    let target = self
                        .typed
                        .bodies
                        .get(body)
                        .and_then(|semantics| semantics.expression_resolutions.get(expression))
                        .and_then(|resolution| match resolution {
                            Resolution::Definition(target)
                                if matches!(
                                    self.program.definitions[*target].kind,
                                    DefinitionKind::Constant(_)
                                ) =>
                            {
                                Some(*target)
                            }
                            _ => None,
                        });
                    let Some(target) = target else {
                        continue;
                    };
                    if active.contains(&target)
                        || max_seen_depth
                            .get(&target)
                            .is_some_and(|seen| *seen >= next_depth)
                    {
                        continue;
                    }
                    let DefinitionKind::Constant(target_constant) =
                        self.program.definitions[target].kind.clone()
                    else {
                        unreachable!("constant resolution changed definition kind")
                    };
                    max_seen_depth.insert(target, next_depth);
                    active.insert(target);
                    pending.push(ConstantLimitFrame::Complete(target));
                    pending.push(ConstantLimitFrame::Expression {
                        body: target_constant.value,
                        expression: self.program.bodies[target_constant.value].root,
                        depth: next_depth,
                    });
                }
                Expr::Unary { operand, .. } => pending.push(ConstantLimitFrame::Expression {
                    body,
                    expression: operand,
                    depth: next_depth,
                }),
                Expr::Binary { left, right, .. } => {
                    pending.push(ConstantLimitFrame::Expression {
                        body,
                        expression: right,
                        depth: next_depth,
                    });
                    pending.push(ConstantLimitFrame::Expression {
                        body,
                        expression: left,
                        depth: next_depth,
                    });
                }
                Expr::Literal(_)
                | Expr::Tuple(_)
                | Expr::List(_)
                | Expr::SelfValue
                | Expr::ResultValue
                | Expr::Old(_)
                | Expr::Block { .. }
                | Expr::If { .. }
                | Expr::Match { .. }
                | Expr::Call { .. }
                | Expr::MethodCall { .. }
                | Expr::QualifiedMethodCall { .. }
                | Expr::Field { .. }
                | Expr::Assign { .. }
                | Expr::RecordLiteral { .. }
                | Expr::Await(_)
                | Expr::Propagate(_)
                | Expr::Return(_)
                | Expr::Error => {}
            }
        }
        true
    }

    fn evaluate_constant(
        &mut self,
        definition: DefId,
        states: &mut BTreeMap<DefId, ConstantEvaluationState>,
    ) -> Option<ConstantValue> {
        match states.get(&definition).copied() {
            Some(ConstantEvaluationState::Complete) => {
                return self.typed.constants.get(definition).cloned();
            }
            Some(ConstantEvaluationState::Failed) => return None,
            Some(ConstantEvaluationState::Evaluating) => {
                self.error(
                    "ConstantCycle",
                    "constant definitions form an evaluation cycle",
                    self.definition_span(definition),
                );
                states.insert(definition, ConstantEvaluationState::Failed);
                return None;
            }
            None => {}
        }
        let DefinitionKind::Constant(constant) = self.program.definitions[definition].kind.clone()
        else {
            return None;
        };
        states.insert(definition, ConstantEvaluationState::Evaluating);
        let root = self.program.bodies[constant.value].root;
        let value = self.evaluate_constant_expr(constant.value, root, states);
        if let Some(value) = value {
            self.typed.constants.insert(definition, value.clone());
            states.insert(definition, ConstantEvaluationState::Complete);
            Some(value)
        } else {
            states.insert(definition, ConstantEvaluationState::Failed);
            None
        }
    }

    #[allow(clippy::too_many_lines)]
    fn evaluate_constant_expr(
        &mut self,
        body: BodyId,
        expression: ExprId,
        states: &mut BTreeMap<DefId, ConstantEvaluationState>,
    ) -> Option<ConstantValue> {
        let source = self.program.bodies[body].expressions[expression].clone();
        match source {
            Expr::Literal(Literal::Bool(value)) => Some(ConstantValue::Bool(value)),
            Expr::Literal(Literal::Int(value)) => value
                .parse::<i64>()
                .ok()
                .map(ConstantValue::Int)
                .or_else(|| {
                    self.constant_evaluation_error(body, expression, "invalid Int constant");
                    None
                }),
            Expr::Literal(Literal::Float(value)) => value
                .parse::<f64>()
                .ok()
                .map(ConstantValue::Float)
                .or_else(|| {
                    self.constant_evaluation_error(body, expression, "invalid Float constant");
                    None
                }),
            Expr::Literal(Literal::Text(value)) => decode_constant_text(&value)
                .ok()
                .map(ConstantValue::Text)
                .or_else(|| {
                    self.constant_evaluation_error(body, expression, "invalid Text constant");
                    None
                }),
            Expr::Path(_) => {
                let resolution = self
                    .typed
                    .bodies
                    .get(body)
                    .and_then(|semantics| semantics.expression_resolutions.get(expression))
                    .copied();
                match resolution {
                    Some(Resolution::Definition(target))
                        if matches!(
                            self.program.definitions[target].kind,
                            DefinitionKind::Constant(_)
                        ) =>
                    {
                        self.evaluate_constant(target, states)
                    }
                    _ => {
                        self.invalid_constant_expression(body, expression);
                        None
                    }
                }
            }
            Expr::Unary { op, operand } => {
                if op == UnaryOp::Negate
                    && matches!(
                        &self.program.bodies[body].expressions[operand],
                        Expr::Literal(Literal::Int(value)) if value == "9223372036854775808"
                    )
                {
                    return Some(ConstantValue::Int(i64::MIN));
                }
                let operand = self.evaluate_constant_expr(body, operand, states)?;
                let value = match (op, operand) {
                    (UnaryOp::Not, ConstantValue::Bool(value)) => Some(ConstantValue::Bool(!value)),
                    (UnaryOp::Negate, ConstantValue::Int(value)) => {
                        value.checked_neg().map(ConstantValue::Int)
                    }
                    (UnaryOp::Negate, ConstantValue::Float(value)) => {
                        Some(ConstantValue::Float(-value))
                    }
                    _ => None,
                };
                value.or_else(|| {
                    self.constant_evaluation_error(
                        body,
                        expression,
                        "constant unary operation cannot be evaluated",
                    );
                    None
                })
            }
            Expr::Binary { op, left, right } => {
                let left = self.evaluate_constant_expr(body, left, states)?;
                match (op, left) {
                    (BinaryOp::And, ConstantValue::Bool(false)) => Some(ConstantValue::Bool(false)),
                    (BinaryOp::Or, ConstantValue::Bool(true)) => Some(ConstantValue::Bool(true)),
                    (op, left) => {
                        let right = self.evaluate_constant_expr(body, right, states)?;
                        self.evaluate_constant_binary(body, expression, op, left, right)
                    }
                }
            }
            Expr::Literal(Literal::Unit)
            | Expr::Tuple(_)
            | Expr::List(_)
            | Expr::SelfValue
            | Expr::ResultValue
            | Expr::Old(_)
            | Expr::Block { .. }
            | Expr::If { .. }
            | Expr::Match { .. }
            | Expr::Call { .. }
            | Expr::MethodCall { .. }
            | Expr::QualifiedMethodCall { .. }
            | Expr::Field { .. }
            | Expr::Assign { .. }
            | Expr::RecordLiteral { .. }
            | Expr::Await(_)
            | Expr::Propagate(_)
            | Expr::Return(_)
            | Expr::Error => {
                self.invalid_constant_expression(body, expression);
                None
            }
        }
    }

    #[allow(clippy::float_cmp, clippy::too_many_lines)]
    fn evaluate_constant_binary(
        &mut self,
        body: BodyId,
        expression: ExprId,
        op: BinaryOp,
        left: ConstantValue,
        right: ConstantValue,
    ) -> Option<ConstantValue> {
        let value = match (left, right) {
            (ConstantValue::Bool(left), ConstantValue::Bool(right)) => match op {
                BinaryOp::And => Some(ConstantValue::Bool(left && right)),
                BinaryOp::Or => Some(ConstantValue::Bool(left || right)),
                BinaryOp::Equal => Some(ConstantValue::Bool(left == right)),
                BinaryOp::NotEqual => Some(ConstantValue::Bool(left != right)),
                _ => None,
            },
            (ConstantValue::Int(left), ConstantValue::Int(right)) => match op {
                BinaryOp::Add => left.checked_add(right).map(ConstantValue::Int),
                BinaryOp::Subtract => left.checked_sub(right).map(ConstantValue::Int),
                BinaryOp::Multiply => left.checked_mul(right).map(ConstantValue::Int),
                BinaryOp::Divide => left.checked_div(right).map(ConstantValue::Int),
                BinaryOp::Equal => Some(ConstantValue::Bool(left == right)),
                BinaryOp::NotEqual => Some(ConstantValue::Bool(left != right)),
                BinaryOp::Less => Some(ConstantValue::Bool(left < right)),
                BinaryOp::LessEqual => Some(ConstantValue::Bool(left <= right)),
                BinaryOp::Greater => Some(ConstantValue::Bool(left > right)),
                BinaryOp::GreaterEqual => Some(ConstantValue::Bool(left >= right)),
                BinaryOp::And | BinaryOp::Or => None,
            },
            (ConstantValue::Float(left), ConstantValue::Float(right)) => match op {
                BinaryOp::Add => Some(ConstantValue::Float(left + right)),
                BinaryOp::Subtract => Some(ConstantValue::Float(left - right)),
                BinaryOp::Multiply => Some(ConstantValue::Float(left * right)),
                BinaryOp::Divide => Some(ConstantValue::Float(left / right)),
                BinaryOp::Equal => Some(ConstantValue::Bool(left == right)),
                BinaryOp::NotEqual => Some(ConstantValue::Bool(left != right)),
                BinaryOp::Less => Some(ConstantValue::Bool(left < right)),
                BinaryOp::LessEqual => Some(ConstantValue::Bool(left <= right)),
                BinaryOp::Greater => Some(ConstantValue::Bool(left > right)),
                BinaryOp::GreaterEqual => Some(ConstantValue::Bool(left >= right)),
                BinaryOp::And | BinaryOp::Or => None,
            },
            (ConstantValue::Text(left), ConstantValue::Text(right)) => match op {
                BinaryOp::Equal => Some(ConstantValue::Bool(left == right)),
                BinaryOp::NotEqual => Some(ConstantValue::Bool(left != right)),
                _ => None,
            },
            _ => None,
        };
        value.or_else(|| {
            self.constant_evaluation_error(
                body,
                expression,
                "constant binary operation cannot be evaluated",
            );
            None
        })
    }

    fn invalid_constant_expression(&mut self, body: BodyId, expression: ExprId) {
        let span = self.program.bodies[body]
            .source_map
            .expr(expression)
            .unwrap_or_default();
        self.error(
            "InvalidConstantExpression",
            "a constant initializer may contain only primitive literals, constant references, and unary or binary constant operations",
            span,
        );
    }

    fn constant_evaluation_error(
        &mut self,
        body: BodyId,
        expression: ExprId,
        message: &'static str,
    ) {
        let span = self.program.bodies[body]
            .source_map
            .expr(expression)
            .unwrap_or_default();
        self.error("ConstantEvaluationFailed", message, span);
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
            BodyKind::Constant => {
                let ty = match self.typed.signatures.get(owner) {
                    Some(Signature::Constant { ty }) => *ty,
                    _ => self.typed.types.error(),
                };
                (ty, ty, None, None, None, ContractMode::None)
            }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InvariantMutation {
    Assignment,
    ReceiverCall,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ActiveBorrow {
    owner: Place,
    mutable: bool,
    region: RegionId,
    identity: BorrowIdentity,
    span: Span,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum BorrowIdentity {
    Interface(ViewTokenId),
    InOut(u32),
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
    pending_must_scope_locals: BTreeSet<LocalId>,
    transferred_must_scope_locals: BTreeSet<LocalId>,
    active_no_suspend: Vec<(LocalId, RegionId, Span)>,
    proof_facts: ProofFacts,
    local_terms: BTreeMap<LocalId, ProofTerm>,
    task_obligations: BTreeMap<TaskObligationOwner, TaskObligationState>,
}

struct LoopFlowTarget {
    cleanup_depth: u32,
    defer_base: usize,
    breaks: Vec<FlowState>,
    continues: Vec<FlowState>,
}

#[derive(Clone)]
struct DeferredFlowEffect {
    makes_self_dirty: bool,
    self_boundaries: BTreeSet<ExprId>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum TaskObligationOwner {
    Local(LocalId),
    Param(ParamId),
    SelfValue,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TaskObligationState {
    Live,
    Consumed,
    Conditional,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TaskJoinPolicy {
    All,
    Settled,
    Any,
    Race,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ValuePathLookup {
    Missing,
    Bound,
    Invalid,
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
    checking_discard_operand: bool,
    dynamic_coercion_mode: DynamicCoercionMode,
    regions: Vec<RegionId>,
    next_region: u32,
    next_view_token: u32,
    next_inout_scope: u32,
    borrows: Vec<ActiveBorrow>,
    scoped_locals: BTreeSet<LocalId>,
    pending_must_scope_locals: BTreeSet<LocalId>,
    transferred_must_scope_locals: BTreeSet<LocalId>,
    active_no_suspend: Vec<(LocalId, RegionId, Span)>,
    task_obligations: BTreeMap<TaskObligationOwner, TaskObligationState>,
    cleanup_depth: u32,
    active_defer_effects: Vec<DeferredFlowEffect>,
    current_defer_self_boundaries: BTreeSet<ExprId>,
    reported_defer_self_boundaries: BTreeSet<ExprId>,
    loop_flows: Vec<LoopFlowTarget>,
    allow_await_here: bool,
    checking_scoped_receiver: bool,
    scoped_initializer: Option<ExprId>,
    proof_facts: ProofFacts,
    local_terms: BTreeMap<LocalId, ProofTerm>,
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
            checking_discard_operand: false,
            dynamic_coercion_mode: DynamicCoercionMode::Owned,
            regions: vec![RegionId(0)],
            next_region: 1,
            next_view_token: 0,
            next_inout_scope: 0,
            borrows: Vec::new(),
            scoped_locals: BTreeSet::new(),
            pending_must_scope_locals: BTreeSet::new(),
            transferred_must_scope_locals: BTreeSet::new(),
            active_no_suspend: Vec::new(),
            task_obligations: BTreeMap::new(),
            cleanup_depth: 0,
            active_defer_effects: Vec::new(),
            current_defer_self_boundaries: BTreeSet::new(),
            reported_defer_self_boundaries: BTreeSet::new(),
            loop_flows: Vec::new(),
            allow_await_here: false,
            checking_scoped_receiver: false,
            scoped_initializer: None,
            proof_facts: ProofFacts::default(),
            local_terms: BTreeMap::new(),
        }
    }

    fn check(&mut self) {
        self.seed_entry_proofs();
        self.seed_task_obligations();
        if self.environment.is_async
            && self.has_task_obligation(self.environment.return_ty, &mut BTreeSet::new(), 0)
        {
            self.error(
                "TaskAsyncResultUnsupported",
                "an async callable cannot complete with a Task-carrying result before runtime reparenting is available",
                self.body_span(),
            );
        }
        let root = self.source().root;
        self.check_expr(
            root,
            Some(self.environment.expected_root),
            ExpressionContext::Value,
        );
        self.check_parameter_task_obligations();
        self.classify_contract_body();
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
            Expr::Field { receiver, name } => {
                self.check_field(expression, receiver, &name, expected)
            }
            Expr::Unary { op, operand } => self.check_unary(expression, op, operand),
            Expr::Binary { op, left, right } => self.check_binary(expression, op, left, right),
            Expr::Assign { target, value } => self.check_assignment(expression, target, value),
            Expr::RecordLiteral { ty, fields } => {
                self.check_record_literal(expression, &ty, &fields, expected)
            }
            Expr::Await(value) => self.check_await(expression, value, await_allowed),
            Expr::Propagate(value) => self.check_propagate(expression, value),
            Expr::Return(value) => self.check_return(value),
        };
        let result = if let Some(expected) = expected {
            self.coerce(expression, inferred, expected)
        } else {
            inferred
        };
        self.semantics.expression_types.insert(expression, result);
        self.invalidate_mutated_receiver(expression);
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
        self.consume_task_obligation(value);
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

    fn check_task_join(
        &mut self,
        expression: ExprId,
        mode: TaskJoinPolicy,
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
        for argument in arguments {
            self.consume_task_obligation(*argument);
        }
        self.finish_call_arguments(arguments);

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
                TaskJoinPolicy::All => {
                    let list = self.types().intern(TyData::List(output));
                    self.types().intern(TyData::Task(list))
                }
                TaskJoinPolicy::Settled => {
                    let outcome = self.types().intern(TyData::TaskOutcome(output));
                    let list = self.types().intern(TyData::List(outcome));
                    self.types().intern(TyData::Task(list))
                }
                TaskJoinPolicy::Any => self.types().intern(TyData::Task(output)),
                TaskJoinPolicy::Race => {
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
            TaskJoinPolicy::All => self.types().intern(TyData::Tuple(outputs)),
            TaskJoinPolicy::Settled => {
                let outcomes = outputs
                    .into_iter()
                    .map(|output| self.types().intern(TyData::TaskOutcome(output)))
                    .collect();
                self.types().intern(TyData::Tuple(outcomes))
            }
            TaskJoinPolicy::Any | TaskJoinPolicy::Race => {
                let first = outputs[0];
                if outputs.iter().any(|output| *output != first) {
                    self.error_at(
                        "HeterogeneousFirstTaskJoin",
                        "Task.any and Task.race require one common result type",
                        expression,
                    );
                }
                if mode == TaskJoinPolicy::Race {
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
        if self.has_task_obligation(operand, &mut BTreeSet::new(), 0) {
            self.consume_task_obligation(value);
        }
        self.diagnose_task_obligations_at_possible_exit();
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
                    if self.transferred_must_scope_locals.contains(&local)
                        && !self.checking_discard_operand
                    {
                        self.error_at(
                            "MustScopeAlreadyTransferred",
                            "this resource was already transferred into a scoped binding",
                            expression,
                        );
                    }
                    if self.pending_must_scope_locals.contains(&local) {
                        if self.scoped_initializer == Some(expression) {
                            self.pending_must_scope_locals.remove(&local);
                            self.transferred_must_scope_locals.insert(local);
                        } else if !self.checking_discard_operand {
                            self.error_at(
                                "MustScopeRequiresScoped",
                                "a resource extracted from a wrapper must be moved directly into `scoped`",
                                expression,
                            );
                        }
                    }
                    if self.scoped_locals.contains(&local)
                        && !self.checking_assignment_target
                        && !self.checking_scoped_receiver
                        && !self.checking_discard_operand
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
                    if !self.checking_assignment_target {
                        self.check_borrowed_place_use(&place, PlaceAccess::Read, expression);
                    }
                    self.semantics.expression_places.insert(expression, place);
                    return ty;
                }
            }
            if let Some((parameter, ty)) = self.environment.params.get(name).copied() {
                self.reject_task_exit_contract_input(expression, ty);
                self.semantics
                    .expression_resolutions
                    .insert(expression, Resolution::Param(parameter));
                let place = Place {
                    root: PlaceRoot::Param(parameter),
                    projections: Vec::new(),
                    mutability: Mutability::ReadOnly,
                };
                if !self.checking_assignment_target {
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
            if matches!(
                self.analyzer.program.definitions[definition].kind,
                DefinitionKind::Constant(_)
            ) {
                return match self.analyzer.typed.signatures.get(definition) {
                    Some(Signature::Constant { ty }) => *ty,
                    _ => self.types().error(),
                };
            }
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
        self.reject_task_exit_contract_input(expression, ty);
        if self.cleanup_depth > 0
            && !self.allow_dirty_self_projection
            && !self.checking_assignment_target
        {
            self.current_defer_self_boundaries.insert(expression);
        }
        if self.self_dirty && !self.allow_dirty_self_projection && !self.checking_assignment_target
        {
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
        if !self.checking_assignment_target {
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
        let defer_base = self.active_defer_effects.len();
        let mut diverges = false;
        for statement in statements {
            let was_diverged = diverges;
            let unreachable_flow = was_diverged.then(|| self.flow_state());
            let unreachable_defer_count = self.active_defer_effects.len();
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
                    let previous_initializer = self.scoped_initializer;
                    if scoped {
                        self.scoped_initializer = Some(*value);
                    }
                    let ty =
                        self.check_suspendable_expr(*value, expected, ExpressionContext::Value);
                    self.scoped_initializer = previous_initializer;
                    diverges |= self.analyzer.typed.types.data(ty) == &TyData::Never;
                    self.consume_task_obligation(*value);
                    self.semantics.local_types.insert(*local, ty);
                    self.register_task_local(*local, ty);
                    let term = self.proof_term(*value);
                    if term.is_known() {
                        self.local_terms.insert(*local, term);
                    } else {
                        self.local_terms.remove(local);
                    }
                    let local_term = ProofTerm::Place(ProofPlace {
                        root: ProofRoot::Local(local.raw()),
                        fields: Vec::new(),
                    });
                    self.assume_established_type(ty, &local_term);
                    if scoped {
                        self.check_scoped_binding(*local, *value, ty, region);
                    } else if self.has_must_scope_obligation_root(ty) {
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
                    self.consume_task_obligation(*value);
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
                    let element_terms = match self.proof_term(*value) {
                        ProofTerm::Tuple(elements) if elements.len() == locals.len() => elements,
                        _ => vec![ProofTerm::Unknown; locals.len()],
                    };
                    for ((local, element_ty), term) in
                        locals.iter().zip(element_types).zip(element_terms)
                    {
                        self.semantics.local_types.insert(*local, element_ty);
                        self.register_task_local(*local, element_ty);
                        if term.is_known() {
                            self.local_terms.insert(*local, term);
                        }
                        self.assume_established_type(
                            element_ty,
                            &ProofTerm::Place(ProofPlace {
                                root: ProofRoot::Local(local.raw()),
                                fields: Vec::new(),
                            }),
                        );
                        if self.has_must_scope_obligation_root(element_ty) {
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
                    // The body is a repeated slice. Without an induction
                    // proof, no entry fact is stable on every iteration.
                    self.invalidate_all_proofs();
                    let entry = self.flow_state();

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
                    self.loop_flows.push(LoopFlowTarget {
                        cleanup_depth: self.cleanup_depth,
                        defer_base: self.active_defer_effects.len(),
                        breaks: Vec::new(),
                        continues: Vec::new(),
                    });
                    self.check_expr(*body, Some(unit), ExpressionContext::Value);
                    let body_diverges = self.expression_diverges(*body);
                    let iteration = self.flow_state();
                    let loop_flow = self.loop_flows.pop().expect("range loop flow");
                    if !body_diverges {
                        self.check_loop_backedge_state(&entry, &iteration, *body);
                    }
                    for backedge in &loop_flow.continues {
                        self.check_loop_backedge_state(&entry, backedge, *body);
                    }
                    let mut exits = vec![entry];
                    exits.extend(loop_flow.breaks);
                    self.join_flow_states(exits);
                    self.invalidate_all_proofs();
                    let scope = self.scopes.last_mut().expect("loop block scope exists");
                    if let Some(previous) = previous {
                        scope.insert(name, previous);
                    } else {
                        scope.remove(&name);
                    }
                }
                Statement::While { condition, body } => {
                    let bool_ty = self.types().builtin(BuiltinType::Bool);
                    // The condition and body can both be revisited. Start
                    // their single semantic pass without first-iteration-only
                    // facts, then keep those facts invalid after the loop.
                    self.invalidate_all_proofs();
                    let header = self.flow_state();
                    self.check_expr(*condition, Some(bool_ty), ExpressionContext::Value);
                    let condition_diverges = self.expression_diverges(*condition);
                    let condition_exit = self.flow_state();
                    if condition_diverges {
                        // Check the unreachable body from the pre-condition
                        // structured state. A return in the condition audits
                        // and consumes obligations only on its terminating
                        // path; those mutations must not manufacture errors
                        // inside code that can never execute.
                        self.restore_flow(&header);
                        self.invalidate_all_proofs();
                    }
                    let unit = self.types().builtin(BuiltinType::Unit);
                    self.loop_flows.push(LoopFlowTarget {
                        cleanup_depth: self.cleanup_depth,
                        defer_base: self.active_defer_effects.len(),
                        breaks: Vec::new(),
                        continues: Vec::new(),
                    });
                    self.check_expr(*body, Some(unit), ExpressionContext::Value);
                    let body_diverges = self.expression_diverges(*body);
                    let iteration = self.flow_state();
                    let loop_flow = self.loop_flows.pop().expect("while loop flow");
                    if condition_diverges {
                        // The body remains typechecked for diagnostics and
                        // side tables, but it is unreachable and cannot alter
                        // the terminating condition flow.
                        self.restore_flow(&condition_exit);
                        diverges = true;
                    } else {
                        if !body_diverges {
                            self.check_loop_backedge_state(&header, &iteration, *body);
                        }
                        for backedge in &loop_flow.continues {
                            self.check_loop_backedge_state(&header, backedge, *body);
                        }
                        let mut exits = vec![condition_exit];
                        exits.extend(loop_flow.breaks);
                        self.join_flow_states(exits);
                        self.invalidate_all_proofs();
                    }
                }
                Statement::Break { span } | Statement::Continue { span } => {
                    let is_break = matches!(statement, Statement::Break { .. });
                    let keyword = if is_break { "break" } else { "continue" };
                    let Some((loop_cleanup_depth, defer_base)) = self
                        .loop_flows
                        .last()
                        .map(|target| (target.cleanup_depth, target.defer_base))
                    else {
                        self.error(
                            "LoopControlOutsideLoop",
                            format!("`{keyword}` is only valid inside a loop"),
                            *span,
                        );
                        diverges = true;
                        continue;
                    };
                    let valid_target = loop_cleanup_depth == self.cleanup_depth;
                    if !valid_target {
                        self.error(
                            "LoopControlFromCleanup",
                            format!("a defer cleanup cannot `{keyword}` an enclosing loop"),
                            *span,
                        );
                    }
                    if !was_diverged && valid_target {
                        let mut control_state = self.flow_state();
                        self.apply_defer_effects(&mut control_state, defer_base);
                        let target = self.loop_flows.last_mut().expect("checked loop target");
                        if is_break {
                            target.breaks.push(control_state);
                        } else {
                            target.continues.push(control_state);
                        }
                    }
                    diverges = true;
                }
                Statement::Defer { body } => {
                    if self.cleanup_depth > 0 {
                        self.error_at(
                            "CleanupRegistrationInCleanup",
                            "a defer cleanup cannot register another cleanup",
                            *body,
                        );
                    }
                    let registration_flow = self.flow_state();
                    let registration_defer_count = self.active_defer_effects.len();
                    let previous_boundaries =
                        std::mem::take(&mut self.current_defer_self_boundaries);
                    // A cleanup is a late-read action, not an expression run
                    // at registration. Check it without current value proofs
                    // and restore the complete registration flow afterward.
                    self.invalidate_all_proofs();
                    self.self_dirty = false;
                    self.cleanup_depth = self.cleanup_depth.saturating_add(1);
                    let unit = self.types().builtin(BuiltinType::Unit);
                    self.check_expr(*body, Some(unit), ExpressionContext::Value);
                    self.cleanup_depth = self.cleanup_depth.saturating_sub(1);
                    let makes_self_dirty = self.self_dirty;
                    let self_boundaries = std::mem::replace(
                        &mut self.current_defer_self_boundaries,
                        previous_boundaries,
                    );
                    self.restore_flow(&registration_flow);
                    self.active_defer_effects.truncate(registration_defer_count);
                    self.active_defer_effects.push(DeferredFlowEffect {
                        makes_self_dirty,
                        self_boundaries,
                    });
                }
                Statement::Discard(expression) => {
                    let previous = self.checking_discard_operand;
                    self.checking_discard_operand = true;
                    let ty =
                        self.check_suspendable_expr(*expression, None, ExpressionContext::Value);
                    self.checking_discard_operand = previous;
                    if self.analyzer.typed.types.data(ty) == &TyData::Never {
                        diverges = true;
                    } else if self.is_error(ty) {
                        // The operand already owns its diagnostic.
                    } else if self.has_must_scope_obligation_root(ty) {
                        self.error_at(
                            "MustScopeRequiresScoped",
                            "a value with a MustScope obligation cannot be discarded",
                            *expression,
                        );
                        self.consume_task_obligation(*expression);
                    } else if self.has_task_obligation(ty, &mut BTreeSet::new(), 0) {
                        self.error_at(
                            "UnawaitedAsyncCall",
                            "a Task cannot be discarded; it must be awaited, joined, or returned",
                            *expression,
                        );
                        self.consume_task_obligation(*expression);
                    } else if self.has_unknown_discard_obligation(ty, &mut BTreeSet::new(), 0) {
                        self.error_at(
                            "CannotDiscardUnknownType",
                            "this type is not statically known to be free of Task or MustScope obligations",
                            *expression,
                        );
                        self.consume_task_obligation(*expression);
                    }
                }
                Statement::Expr(expression) => {
                    let ty = self.check_suspendable_expr(
                        *expression,
                        None,
                        ExpressionContext::UnitStatement,
                    );
                    if self.analyzer.typed.types.data(ty) == &TyData::Never {
                        diverges = true;
                    } else if self.is_error(ty)
                        || self.analyzer.typed.types.data(ty) == &TyData::Builtin(BuiltinType::Unit)
                    {
                        // A failed expression already owns its diagnostic, and
                        // Unit is the ordinary statement result.
                    } else if self.has_must_scope_obligation_root(ty) {
                        self.error_at(
                            "MustScopeRequiresScoped",
                            "discarding a MustScope value would lose its cleanup obligation",
                            *expression,
                        );
                    } else if self.has_task_obligation(ty, &mut BTreeSet::new(), 0) {
                        self.error_at(
                            "UnawaitedAsyncCall",
                            "a Task must be awaited, joined, returned, or explicitly stored for later consumption",
                            *expression,
                        );
                    } else {
                        self.error_at(
                            "UnusedValue",
                            "this value is unused; use it or write `discard` to ignore it explicitly",
                            *expression,
                        );
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
                    let term = self.proof_term(*predicate);
                    let check = if self.proof_facts.prove(&term) == ProofResult::Proven {
                        RuntimeCheck::Proven
                    } else {
                        RuntimeCheck::Runtime
                    };
                    self.semantics.assertion_checks.insert(*predicate, check);
                    self.proof_facts.assume(term, true);
                    if self.recover_receiver_invariant() {
                        self.semantics
                            .receiver_invariant_recoveries
                            .insert(*predicate);
                    }
                }
            }
            if let Some(unreachable_flow) = unreachable_flow {
                self.restore_flow(&unreachable_flow);
                self.active_defer_effects.truncate(unreachable_defer_count);
                diverges = true;
            }
        }
        let unreachable_tail_flow = diverges.then(|| self.flow_state());
        let tail_result = if let Some(tail) = tail {
            self.check_suspendable_expr(tail, expected, ExpressionContext::Value)
        } else {
            self.types().builtin(BuiltinType::Unit)
        };
        if let Some(unreachable_tail_flow) = unreachable_tail_flow {
            self.restore_flow(&unreachable_tail_flow);
        }
        if !diverges
            && let Some(tail) = tail
            && self.may_have_task_obligation(tail_result)
        {
            // A block tail transfers its obligation to the block result even
            // when the caller supplied no expected type. This must happen
            // before closing the block scope or a local returned by the tail
            // would be diagnosed as an implicit drop.
            self.consume_task_obligation(tail);
        }
        let result = if diverges {
            self.types().never()
        } else {
            tail_result
        };
        if self.active_defer_effects.len() > defer_base {
            let mut exit_state = self.flow_state();
            self.apply_defer_effects(&mut exit_state, defer_base);
            self.restore_flow(&exit_state);
        }
        self.active_defer_effects.truncate(defer_base);
        self.borrows.retain(|borrow| borrow.region != region);
        self.active_no_suspend
            .retain(|(_, active_region, _)| *active_region != region);
        self.check_current_scope_task_obligations();
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
        let Some(dispose) = self.analyzer.canonical_concepts.dispose else {
            self.error(
                "MissingDisposeConcept",
                "`scoped` requires the canonical std.resource.Dispose concept",
                self.local_span(local),
            );
            return;
        };
        let Some(requirement) = self.analyzer.canonical_concepts.dispose_requirement else {
            self.error(
                "InvalidDisposeConcept",
                "std.resource.Dispose must declare `method dispose(mut self)`",
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
                    "{} does not conform to std.resource.Dispose",
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
        let no_suspend = self.analyzer.canonical_concepts.no_suspend;
        if self.has_resource_conformance(ty, no_suspend).is_some() {
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
        let condition_term = self.proof_term(condition);
        // General `if` conditions may contain calls with mutation. A partial
        // symbolic term would then describe a stale pre-call value, so branch
        // facts are admitted only when the complete condition is proof-pure.
        if condition_term.is_known() {
            self.proof_facts.assume(condition_term.clone(), true);
        }
        let then_ty = self.check_expr(then_branch, expected, context);
        let then_diverges = self.expression_diverges(then_branch);
        let then_state = self.flow_state();
        if let Some(else_branch) = else_branch {
            self.restore_flow(&entry);
            if condition_term.is_known() {
                self.proof_facts.assume(condition_term, false);
            }
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
        let short_circuit_entry =
            matches!(operator, BinaryOp::And | BinaryOp::Or).then(|| self.flow_state());
        let right_ty = self.check_expr(right, None, ExpressionContext::Value);
        if let Some(entry) = short_circuit_entry {
            let evaluated = self.flow_state();
            self.join_flow_states([entry, evaluated]);
        }
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
                } else if !self.supports_value_equality(left_ty)
                    || !self.supports_value_equality(right_ty)
                {
                    self.error_at(
                        "InvalidGenericOperation",
                        "equality is unavailable because an operand type does not support value equality",
                        expression,
                    );
                }
                bool_ty
            }
        }
    }

    fn supports_value_equality(&mut self, ty: TyId) -> bool {
        self.supports_value_equality_inner(ty, &mut BTreeSet::new(), 0)
    }

    #[allow(clippy::too_many_lines)]
    fn supports_value_equality_inner(
        &mut self,
        ty: TyId,
        active: &mut BTreeSet<TyId>,
        depth: u16,
    ) -> bool {
        if depth >= 128 {
            return false;
        }
        match self.analyzer.typed.types.data(ty).clone() {
            TyData::Error
            | TyData::Never
            | TyData::Builtin(
                BuiltinType::Bool
                | BuiltinType::Int
                | BuiltinType::Float
                | BuiltinType::Text
                | BuiltinType::Bytes
                | BuiltinType::Path
                | BuiltinType::Unit
                | BuiltinType::ConstraintError
                | BuiltinType::TaskFault
                | BuiltinType::Duration,
            ) => true,
            TyData::Builtin(BuiltinType::ContractFault)
            | TyData::Param(_)
            | TyData::DynTarget(_)
            | TyData::View { .. }
            | TyData::Task(_) => false,
            TyData::SelfType(_) | TyData::Projection { .. } => {
                self.symbolic_concept_contract_equality_allowed()
            }
            TyData::Tuple(elements) => elements
                .into_iter()
                .all(|element| self.supports_value_equality_inner(element, active, depth + 1)),
            TyData::List(element)
            | TyData::TextMap(element)
            | TyData::Option(element)
            | TyData::TaskOutcome(element) => {
                self.supports_value_equality_inner(element, active, depth + 1)
            }
            TyData::Result { ok, error } => {
                self.supports_value_equality_inner(ok, active, depth + 1)
                    && self.supports_value_equality_inner(error, active, depth + 1)
            }
            TyData::Nominal {
                definition,
                arguments,
            } => {
                if [
                    self.analyzer.canonical_std_items.file,
                    self.analyzer.canonical_std_items.io_error,
                    self.analyzer.canonical_std_items.socket,
                ]
                .contains(&Some(definition))
                {
                    return false;
                }
                if !active.insert(ty) {
                    // Recursive values are finite at runtime. Re-entering the
                    // same instantiated nominal therefore closes this
                    // coinductive derivation instead of making equality
                    // unavailable solely because the declaration is recursive.
                    return true;
                }
                let parameters = self.analyzer.type_generic_params(definition);
                let substitution = substitution_for(&parameters, &arguments);
                let kind = self.analyzer.program.definitions[definition].kind.clone();
                let result = match kind {
                    DefinitionKind::RefinedType(refined) => self
                        .analyzer
                        .typed
                        .resolved_type_refs
                        .get(refined.base)
                        .copied()
                        .is_some_and(|base| {
                            let base = self.types().substitute(base, &substitution);
                            self.supports_value_equality_inner(base, active, depth + 1)
                        }),
                    DefinitionKind::Record(record) => record.fields.into_iter().all(|field| {
                        let Some(Signature::Field { ty, .. }) =
                            self.analyzer.typed.signatures.get(field).cloned()
                        else {
                            return false;
                        };
                        let field_ty = self.types().substitute(ty, &substitution);
                        self.supports_value_equality_inner(field_ty, active, depth + 1)
                    }),
                    DefinitionKind::Enum(enumeration) => {
                        enumeration.variants.into_iter().all(|variant| {
                            let Some(Signature::Variant { payload, .. }) =
                                self.analyzer.typed.signatures.get(variant).cloned()
                            else {
                                return false;
                            };
                            payload.into_iter().all(|payload_ty| {
                                let payload_ty = self.types().substitute(payload_ty, &substitution);
                                self.supports_value_equality_inner(payload_ty, active, depth + 1)
                            })
                        })
                    }
                    _ => false,
                };
                active.remove(&ty);
                result
            }
        }
    }

    fn symbolic_concept_contract_equality_allowed(&self) -> bool {
        if self.environment.contract == ContractMode::None {
            return false;
        }
        let DefinitionKind::Method(method) =
            &self.analyzer.program.definitions[self.environment.owner].kind
        else {
            return false;
        };
        matches!(
            self.analyzer.program.definitions[method.owner].kind,
            DefinitionKind::Concept(_)
        )
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
        if self.has_task_obligation(target_ty, &mut BTreeSet::new(), 0) {
            self.error_at(
                "TaskAssignmentUnsupported",
                "assigning over a Task-carrying place is unavailable before task transfer ownership is explicit",
                target,
            );
            let owner = match &place.root {
                PlaceRoot::Local(local) => TaskObligationOwner::Local(*local),
                PlaceRoot::Param(parameter) => TaskObligationOwner::Param(*parameter),
                PlaceRoot::SelfValue => TaskObligationOwner::SelfValue,
            };
            self.task_obligations
                .insert(owner, TaskObligationState::Consumed);
        }
        self.check_borrowed_place_use(&place, PlaceAccess::Write, target);
        if !place.projections.is_empty() && !matches!(place.root, PlaceRoot::SelfValue) {
            self.error_at(
                "ReadonlyReceiverMutation",
                "record fields can only be changed through the owning type's mut self method",
                target,
            );
        }
        let dirties_self = matches!(place.root, PlaceRoot::SelfValue)
            && self.check_invariant_mutation_boundary(
                target,
                &place,
                InvariantMutation::Assignment,
            );
        self.check_expr(value, Some(target_ty), ExpressionContext::Value);
        let value_term = self.proof_term(value);
        let proof_place = proof_place(&place);
        self.proof_facts.invalidate(&proof_place);
        self.local_terms.retain(|local, term| {
            matches!(place.root, PlaceRoot::Local(target) if *local == target)
                || !term.contains_place(&proof_place)
        });
        if let PlaceRoot::Local(local) = place.root
            && place.projections.is_empty()
        {
            // A term mentioning the destination denotes its old value. Do not
            // let that spelling become a self-reference after assignment.
            if value_term.is_known() && !value_term.contains_place(&proof_place) {
                self.local_terms.insert(local, value_term);
            } else {
                self.local_terms.remove(&local);
            }
        }
        let established = ProofTerm::Place(proof_place);
        self.assume_established_type(target_ty, &established);
        if matches!(place.root, PlaceRoot::SelfValue) && place.projections.is_empty() {
            self.self_dirty = false;
        } else if dirties_self {
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
            if self.may_have_task_obligation(return_ty) {
                self.consume_task_obligation(value);
            }
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
        self.audit_task_obligations_at_exit();
        self.types().never()
    }

    fn check_field(
        &mut self,
        expression: ExprId,
        receiver: ExprId,
        name: &Name,
        expected: Option<TyId>,
    ) -> TyId {
        if let Expr::Path(type_path) = self.source().expressions[receiver].clone()
            && self.value_path_lookup(&type_path) == ValuePathLookup::Missing
            && let Some((variant, owner)) = self.resolve_qualified_variant(&type_path, name)
        {
            return self.check_variant_constructor(expression, variant, owner, None, &[], expected);
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
        if self.environment.contract == ContractMode::None {
            self.consume_task_obligation(scrutinee);
        } else {
            self.borrow_task_obligation(scrutinee);
        }
        let mut result = expected;
        let mut pattern_rows = Vec::new();
        let entry = self.flow_state();
        let mut exits = Vec::new();
        let mut has_reachable_arm = false;
        for arm in arms {
            self.restore_flow(&entry);
            self.scopes.push(BTreeMap::new());
            let pattern = self.check_pattern(arm.pattern, scrutinee_ty);
            let useful = self.pattern_is_useful(scrutinee_ty, &pattern_rows, &pattern);
            if !useful {
                self.error(
                    "UnreachableMatchArm",
                    "match arm is unreachable because previous arms already cover it",
                    self.pattern_span(arm.pattern),
                );
            }
            if !matches!(pattern, CheckedPattern::Invalid) {
                pattern_rows.push(vec![pattern]);
            }
            let arm_ty = self.check_expr(arm.value, result, context);
            if self.may_have_task_obligation(arm_ty) {
                // Each ordinary reachable arm transfers its value into the
                // match result. Contract arms instead borrow-check that same
                // value without changing the surrounding ownership state.
                if self.environment.contract == ContractMode::None {
                    self.consume_task_obligation(arm.value);
                } else {
                    self.borrow_task_obligation(arm.value);
                }
            }
            let arm_locals = self
                .scopes
                .last()
                .into_iter()
                .flat_map(BTreeMap::values)
                .copied()
                .collect::<Vec<_>>();
            for local in arm_locals {
                if self.pending_must_scope_locals.remove(&local) {
                    self.error(
                        "MustScopeRequiresScoped",
                        "a resource extracted by this pattern must be moved directly into `scoped`",
                        self.local_span(local),
                    );
                }
            }
            self.check_current_scope_task_obligations();
            self.scopes.pop();
            if useful && !self.expression_diverges(arm.value) {
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
        self.check_exhaustive(scrutinee_ty, &pattern_rows, scrutinee);
        if !has_reachable_arm && !arms.is_empty() {
            self.types().never()
        } else {
            result.unwrap_or_else(|| self.types().error())
        }
    }

    #[allow(clippy::too_many_lines)]
    fn check_pattern(&mut self, pattern: PatternId, expected: TyId) -> CheckedPattern {
        self.semantics.pattern_types.insert(pattern, expected);
        match self.source().patterns[pattern].clone() {
            Pattern::Error => {
                self.semantics
                    .pattern_resolutions
                    .insert(pattern, Resolution::Error);
                CheckedPattern::Invalid
            }
            Pattern::Wildcard => {
                if self.has_must_scope_obligation_root(expected) {
                    self.error(
                        "MustScopeRequiresScoped",
                        "a pattern cannot discard a value containing a MustScope resource",
                        self.pattern_span(pattern),
                    );
                }
                if self.environment.contract == ContractMode::None
                    && self.may_have_task_obligation(expected)
                {
                    self.error(
                        "TaskPatternDiscard",
                        "a pattern cannot discard a value carrying a Task obligation",
                        self.pattern_span(pattern),
                    );
                }
                CheckedPattern::Wildcard
            }
            Pattern::Binding(local) => {
                self.bind_pattern(pattern, local, expected);
                CheckedPattern::Wildcard
            }
            Pattern::Literal(literal) => {
                let literal_ty = match literal {
                    Literal::Bool(_) => self.types().builtin(BuiltinType::Bool),
                    Literal::Int(_) => self.types().builtin(BuiltinType::Int),
                    Literal::Float(_) => self.types().builtin(BuiltinType::Float),
                    Literal::Text(_) => self.types().builtin(BuiltinType::Text),
                    Literal::Unit => self.types().builtin(BuiltinType::Unit),
                };
                self.expect_compatible(pattern, literal_ty, expected);
                CheckedPattern::Literal(literal)
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
                    let payload_types = self.variant_payload(variant, expected);
                    if payload_types.len() != payload.len() {
                        self.error(
                            "TypeMismatch",
                            "variant pattern payload has the wrong arity",
                            self.pattern_span(pattern),
                        );
                    }
                    let valid_arity = payload_types.len() == payload.len();
                    let children = payload
                        .iter()
                        .enumerate()
                        .map(|(index, child)| {
                            let ty = payload_types
                                .get(index)
                                .copied()
                                .unwrap_or_else(|| self.types().error());
                            self.check_pattern(*child, ty)
                        })
                        .collect();
                    if valid_arity {
                        CheckedPattern::Variant(variant, children)
                    } else {
                        CheckedPattern::Invalid
                    }
                } else if let Some(binding) = binding {
                    self.bind_pattern(pattern, binding, expected);
                    CheckedPattern::Wildcard
                } else {
                    self.error(
                        "UnknownName",
                        format!("unknown variant `{}`", path.as_string()),
                        self.pattern_span(pattern),
                    );
                    self.semantics
                        .pattern_resolutions
                        .insert(pattern, Resolution::Error);
                    CheckedPattern::Invalid
                }
            }
            Pattern::Variant { path, payload } => {
                if let Some(variant) = self.resolve_pattern_variant(&path, &payload, expected) {
                    self.semantics
                        .pattern_resolutions
                        .insert(pattern, pattern_variant_resolution(variant));
                    let payload_types = self.variant_payload(variant, expected);
                    if payload_types.len() != payload.len() {
                        self.error(
                            "TypeMismatch",
                            "variant pattern payload has the wrong arity",
                            self.pattern_span(pattern),
                        );
                    }
                    let valid_arity = payload_types.len() == payload.len();
                    let children = payload
                        .iter()
                        .enumerate()
                        .map(|(index, child)| {
                            let ty = payload_types
                                .get(index)
                                .copied()
                                .unwrap_or_else(|| self.types().error());
                            self.check_pattern(*child, ty)
                        })
                        .collect();
                    if valid_arity {
                        CheckedPattern::Variant(variant, children)
                    } else {
                        CheckedPattern::Invalid
                    }
                } else {
                    self.error(
                        "UnknownName",
                        format!("unknown variant `{}`", path.as_string()),
                        self.pattern_span(pattern),
                    );
                    self.semantics
                        .pattern_resolutions
                        .insert(pattern, Resolution::Error);
                    CheckedPattern::Invalid
                }
            }
        }
    }

    fn bind_pattern(&mut self, pattern: PatternId, local: LocalId, expected: TyId) {
        self.semantics
            .pattern_resolutions
            .insert(pattern, Resolution::Local(local));
        self.semantics.local_types.insert(local, expected);
        self.register_task_local(local, expected);
        if self.has_must_scope_obligation_root(expected) {
            self.pending_must_scope_locals.insert(local);
        }
        self.assume_established_type(
            expected,
            &ProofTerm::Place(ProofPlace {
                root: ProofRoot::Local(local.raw()),
                fields: Vec::new(),
            }),
        );
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
        if self.reject_dyn_obligation(actual, expression) {
            return self.types().error();
        }
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
                let owner = owner.as_ref().expect("borrowed interface has an owner");
                let _ = self.register_borrow(
                    owner.clone(),
                    mutability == Mutability::Mutable,
                    region,
                    BorrowIdentity::Interface(token),
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

    fn reject_dyn_obligation(&mut self, concrete: TyId, expression: ExprId) -> bool {
        let message = if self.has_must_scope_obligation_root(concrete) {
            Some("a value with a MustScope obligation cannot be erased to dyn")
        } else if self.has_task_obligation(concrete, &mut BTreeSet::new(), 0) {
            Some("a value containing an unconsumed Task cannot be erased to dyn")
        } else if self.has_unknown_discard_obligation(concrete, &mut BTreeSet::new(), 0) {
            Some("a type with unresolved generic obligations cannot be erased to dyn")
        } else {
            None
        };
        if let Some(message) = message {
            self.error_at("IllegalDynConversion", message, expression);
            self.consume_task_obligation_with_type(expression, concrete);
            true
        } else {
            false
        }
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
        let _ = self.register_borrow(
            owner.clone(),
            mutability == Mutability::Mutable,
            region,
            BorrowIdentity::Interface(token),
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
            TyData::TextMap(value) => format!("TextMap[{}]", self.type_name(*value)),
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

    fn canonical_std_type(
        &mut self,
        definition: Option<DefId>,
        qualified_name: &'static str,
        expression: ExprId,
    ) -> TyId {
        let Some(definition) = definition else {
            self.error_at(
                "MissingStandardLibraryItem",
                format!("required canonical standard-library item `{qualified_name}` is missing"),
                expression,
            );
            return self.types().error();
        };
        self.types().intern(TyData::Nominal {
            definition,
            arguments: Vec::new(),
        })
    }

    fn canonical_io_error_type(&mut self, expression: ExprId) -> TyId {
        let definition = self.analyzer.canonical_std_items.io_error;
        self.canonical_std_type(definition, "std.io.IoError", expression)
    }

    fn canonical_file_type(&mut self, expression: ExprId) -> TyId {
        let definition = self.analyzer.canonical_std_items.file;
        self.canonical_std_type(definition, "std.file.File", expression)
    }

    fn canonical_socket_type(&mut self, expression: ExprId) -> TyId {
        let definition = self.analyzer.canonical_std_items.socket;
        self.canonical_std_type(definition, "std.net.Socket", expression)
    }

    fn is_canonical_resource_type(&self, ty: TyId) -> bool {
        matches!(
            self.analyzer.typed.types.data(ty),
            TyData::Nominal {
                definition,
                arguments,
            } if arguments.is_empty()
                && (Some(*definition) == self.analyzer.canonical_std_items.file
                    || Some(*definition) == self.analyzer.canonical_std_items.socket)
        )
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

    fn param_span(&self, parameter: ParamId) -> Span {
        self.analyzer
            .program
            .source_map
            .param(parameter)
            .unwrap_or_default()
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
    fn seed_entry_proofs(&mut self) {
        let body_kind = self.source().kind;
        let parameters = self
            .environment
            .params
            .values()
            .copied()
            .collect::<Vec<_>>();
        for (parameter, ty) in &parameters {
            let term = ProofTerm::Place(ProofPlace {
                root: ProofRoot::Param(parameter.raw()),
                fields: Vec::new(),
            });
            self.assume_established_type(*ty, &term);
            if body_kind == BodyKind::Ensures {
                // Parameters are immutable, but `old(parameter)` has a
                // distinct proof spelling so entry snapshots never become
                // interchangeable with a future mutable value category.
                self.assume_established_type(
                    *ty,
                    &ProofTerm::Place(ProofPlace {
                        root: ProofRoot::OldParam(parameter.raw()),
                        fields: Vec::new(),
                    }),
                );
            }
        }

        if let Some(self_ty) = self.environment.self_ty {
            let self_term = ProofTerm::Place(ProofPlace {
                root: ProofRoot::SelfValue,
                fields: Vec::new(),
            });
            if body_kind == BodyKind::RecordInvariant {
                // The invariant cannot prove itself, but declarations on its
                // field types are already established before it runs.
                self.assume_established_record_fields(self_ty, &self_term);
            } else {
                self.assume_established_type(self_ty, &self_term);
            }
            if body_kind == BodyKind::Ensures {
                // Both sides of a method boundary contain established values.
                // Keep their identities separate because `mut self` may have
                // changed between the two points.
                self.assume_established_type(
                    self_ty,
                    &ProofTerm::Place(ProofPlace {
                        root: ProofRoot::OldSelf,
                        fields: Vec::new(),
                    }),
                );
            }
        }

        if body_kind == BodyKind::Ensures
            && let Some(result_ty) = self.environment.result_ty
        {
            self.assume_established_type(
                result_ty,
                &ProofTerm::Place(ProofPlace {
                    root: ProofRoot::ResultValue,
                    fields: Vec::new(),
                }),
            );
        }

        let contract_owner = self.effective_contract_owner();
        let (requires, ensures) = match &self.analyzer.program.definitions[contract_owner].kind {
            DefinitionKind::Function(function) | DefinitionKind::Test(function) => (
                function.signature.contracts.requires.clone(),
                function.signature.contracts.ensures.clone(),
            ),
            DefinitionKind::Method(method) => (
                method.signature.contracts.requires.clone(),
                method.signature.contracts.ensures.clone(),
            ),
            _ => (Vec::new(), Vec::new()),
        };
        let implementation_parameters =
            match self.analyzer.typed.signatures.get(self.environment.owner) {
                Some(Signature::Callable(signature)) => signature
                    .params
                    .iter()
                    .map(|(parameter, _)| *parameter)
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            };
        let contract_parameters = match self.analyzer.typed.signatures.get(contract_owner) {
            Some(Signature::Callable(signature)) => signature
                .params
                .iter()
                .map(|(parameter, _)| *parameter)
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        let current_argument_terms = contract_parameters
            .iter()
            .copied()
            .zip(implementation_parameters.iter().copied())
            .map(|(contract_parameter, implementation_parameter)| {
                (
                    contract_parameter,
                    ProofTerm::Place(ProofPlace {
                        root: ProofRoot::Param(implementation_parameter.raw()),
                        fields: Vec::new(),
                    }),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let old_argument_terms = contract_parameters
            .into_iter()
            .zip(implementation_parameters)
            .map(|(contract_parameter, implementation_parameter)| {
                (
                    contract_parameter,
                    ProofTerm::Place(ProofPlace {
                        root: ProofRoot::OldParam(implementation_parameter.raw()),
                        fields: Vec::new(),
                    }),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let current_self = self.environment.self_ty.map(|_| {
            ProofTerm::Place(ProofPlace {
                root: ProofRoot::SelfValue,
                fields: Vec::new(),
            })
        });
        let old_self = self.environment.self_ty.map(|_| {
            ProofTerm::Place(ProofPlace {
                root: ProofRoot::OldSelf,
                fields: Vec::new(),
            })
        });
        match body_kind {
            BodyKind::Function | BodyKind::Method => {
                for contract in requires {
                    let term = self.contract_proof_term(
                        contract,
                        current_self.as_ref(),
                        &current_argument_terms,
                    );
                    self.proof_facts.assume(term, true);
                }
            }
            BodyKind::Requires => {
                // Requires clauses execute in source order. A preceding
                // retained check, or a preceding statically proven clause,
                // is therefore an independent fact for the next clause.
                for contract in requires.into_iter().take_while(|body| *body != self.body) {
                    let term = self.contract_proof_term(
                        contract,
                        current_self.as_ref(),
                        &current_argument_terms,
                    );
                    self.proof_facts.assume(term, true);
                }
            }
            BodyKind::Ensures => {
                // A mutable receiver's requires clause is an entry fact. It
                // may prove `old(self...)`, never the post-state `self...`.
                // Ordinary parameters are immutable, so retain both their
                // current and explicitly snapshotted spellings.
                let entry_self = if self.environment.receiver == Some(ReceiverKind::Mutable) {
                    old_self.as_ref()
                } else {
                    current_self.as_ref()
                };
                for contract in requires {
                    let current_term =
                        self.contract_proof_term(contract, entry_self, &current_argument_terms);
                    self.proof_facts.assume(current_term, true);
                    let old_term =
                        self.contract_proof_term(contract, old_self.as_ref(), &old_argument_terms);
                    self.proof_facts.assume(old_term, true);
                }
                // Contract bodies owned by this declaration share parameter
                // and old-value roots, so earlier successful ensures clauses
                // can safely discharge later redundant clauses.
                if contract_owner == self.environment.owner {
                    for contract in ensures.into_iter().take_while(|body| *body != self.body) {
                        let term = self.contract_proof_term(contract, None, &BTreeMap::new());
                        self.proof_facts.assume(term, true);
                    }
                }
            }
            BodyKind::Constant | BodyKind::RefinementPredicate | BodyKind::RecordInvariant => {}
        }
    }

    fn effective_contract_owner(&self) -> DefId {
        let owner = self.environment.owner;
        let DefinitionKind::Method(method) = &self.analyzer.program.definitions[owner].kind else {
            return owner;
        };
        if !matches!(
            self.analyzer.program.definitions[method.owner].kind,
            DefinitionKind::Conformance(_)
        ) {
            return owner;
        }
        self.analyzer
            .typed
            .conformances
            .get(method.owner)
            .and_then(|semantics| {
                semantics
                    .methods
                    .iter()
                    .find_map(|(requirement, implementation)| {
                        (*implementation == owner).then_some(*requirement)
                    })
            })
            .unwrap_or(owner)
    }

    fn classify_contract_body(&mut self) {
        if !matches!(
            self.source().kind,
            BodyKind::RefinementPredicate
                | BodyKind::RecordInvariant
                | BodyKind::Requires
                | BodyKind::Ensures
        ) {
            return;
        }
        let term = self.contract_proof_term(self.body, None, &BTreeMap::new());
        self.semantics.contract_check =
            Some(if self.proof_facts.prove(&term) == ProofResult::Proven {
                RuntimeCheck::Proven
            } else {
                RuntimeCheck::Runtime
            });
    }

    fn assume_established_type(&mut self, ty: TyId, value: &ProofTerm) {
        if let Some(term) = self.established_type_term(ty, value) {
            self.proof_facts.assume(term, true);
        }
        self.assume_established_record_fields(ty, value);
    }

    fn recover_receiver_invariant(&mut self) -> bool {
        if !self.self_dirty {
            return false;
        }
        let Some(self_ty) = self.environment.self_ty else {
            return false;
        };
        let value = ProofTerm::Place(ProofPlace {
            root: ProofRoot::SelfValue,
            fields: Vec::new(),
        });
        if self
            .established_type_term(self_ty, &value)
            .is_some_and(|invariant| self.proof_facts.prove(&invariant) == ProofResult::Proven)
        {
            self.self_dirty = false;
            return true;
        }
        false
    }

    fn assume_established_record_fields(&mut self, ty: TyId, value: &ProofTerm) {
        let TyData::Nominal { definition, .. } = self.analyzer.typed.types.data(ty).clone() else {
            return;
        };
        let DefinitionKind::Record(record) = &self.analyzer.program.definitions[definition].kind
        else {
            return;
        };
        let fields = record
            .fields
            .iter()
            .filter_map(|field| match self.analyzer.typed.signatures.get(*field) {
                Some(Signature::Field { ty, .. }) => Some((*field, *ty)),
                _ => None,
            })
            .collect::<Vec<_>>();
        for (field, field_ty) in fields {
            let field_value = value.clone().field(field);
            if let Some(term) = self.established_type_term(field_ty, &field_value) {
                self.proof_facts.assume(term, true);
            }
        }
    }

    fn established_type_term(&self, ty: TyId, value: &ProofTerm) -> Option<ProofTerm> {
        let TyData::Nominal { definition, .. } = self.analyzer.typed.types.data(ty) else {
            return None;
        };
        self.established_definition_term(*definition, value)
    }

    fn established_definition_term(
        &self,
        definition: DefId,
        value: &ProofTerm,
    ) -> Option<ProofTerm> {
        let contract = match &self.analyzer.program.definitions[definition].kind {
            DefinitionKind::RefinedType(refined) => Some(refined.predicate),
            DefinitionKind::Record(record) => record.invariant,
            _ => None,
        }?;
        Some(self.contract_proof_term(contract, Some(value), &BTreeMap::new()))
    }

    fn invalidate_mutated_receiver(&mut self, expression: ExprId) {
        let inout = self
            .semantics
            .calls
            .get(expression)
            .is_some_and(|call| call.receiver == Some(ReceiverPassing::InOut));
        if !inout {
            return;
        }
        let receiver = match &self.source().expressions[expression] {
            Expr::MethodCall { receiver, .. } => Some(*receiver),
            Expr::QualifiedMethodCall { arguments, .. } | Expr::Call { arguments, .. } => {
                arguments.first().copied()
            }
            _ => None,
        };
        let Some(receiver) = receiver else {
            return;
        };
        let Some(place) = self.semantics.expression_places.get(receiver).cloned() else {
            return;
        };
        let Some(ty) = self.semantics.expression_types.get(receiver).copied() else {
            return;
        };
        self.invalidate_mutated_place(&place, ty);
    }

    fn invalidate_mutated_place(&mut self, place: &Place, ty: TyId) {
        let proof_place = proof_place(place);
        self.proof_facts.invalidate(&proof_place);
        self.local_terms
            .retain(|_, term| !term.contains_place(&proof_place));
        if let PlaceRoot::Local(local) = place.root {
            self.local_terms.remove(&local);
        }
        self.assume_established_type(ty, &ProofTerm::Place(proof_place));
    }

    fn checked_value_proof(&self, expression: ExprId, facts: &mut ProofFacts) -> ProofTerm {
        let candidate = self.proof_term(expression).unrefine();
        let value = if candidate.is_known() {
            candidate
        } else {
            ProofTerm::Place(ProofPlace {
                root: ProofRoot::Expression {
                    body: self.body.raw(),
                    expression: expression.raw(),
                },
                fields: Vec::new(),
            })
        };

        if let Some(ty) = self.semantics.expression_types.get(expression).copied()
            && let Some(term) = self.established_type_term(ty, &value)
        {
            facts.assume(term, true);
        }
        if let Some(Coercion::RefinedToBase { refined }) =
            self.semantics.expression_coercions.get(expression)
            && let Some(term) = self.established_definition_term(*refined, &value)
        {
            facts.assume(term, true);
        }
        value
    }

    fn refinement_proof(&self, definition: DefId, expression: ExprId) -> ProofResult {
        let DefinitionKind::RefinedType(refined) =
            &self.analyzer.program.definitions[definition].kind
        else {
            return ProofResult::Unknown;
        };
        let mut facts = self.proof_facts.clone();
        let value = self.checked_value_proof(expression, &mut facts);
        let predicate = self.contract_proof_term(refined.predicate, Some(&value), &BTreeMap::new());
        facts.prove(&predicate)
    }

    fn invariant_proof(&self, definition: DefId, fields: &[(DefId, ExprId)]) -> ProofResult {
        let DefinitionKind::Record(record) = &self.analyzer.program.definitions[definition].kind
        else {
            return ProofResult::Unknown;
        };
        let Some(invariant) = record.invariant else {
            return ProofResult::Proven;
        };
        let mut facts = self.proof_facts.clone();
        let fields = fields
            .iter()
            .map(|(field, expression)| (*field, self.checked_value_proof(*expression, &mut facts)))
            .collect();
        let value = ProofTerm::Record { definition, fields };
        let predicate = self.contract_proof_term(invariant, Some(&value), &BTreeMap::new());
        facts.prove(&predicate)
    }

    fn proof_term(&self, expression: ExprId) -> ProofTerm {
        self.proof_term_inner(expression, 0)
    }

    #[allow(clippy::too_many_lines)]
    fn proof_term_inner(&self, expression: ExprId, depth: u16) -> ProofTerm {
        if depth >= 256 {
            return ProofTerm::Unknown;
        }
        let mut term = match &self.source().expressions[expression] {
            Expr::Literal(Literal::Bool(value)) => ProofTerm::bool(*value),
            Expr::Literal(Literal::Int(value)) => value
                .parse::<i64>()
                .map_or(ProofTerm::Unknown, ProofTerm::int),
            Expr::Literal(Literal::Float(value)) => value
                .parse::<f64>()
                .map_or(ProofTerm::Unknown, ProofTerm::float),
            Expr::Literal(Literal::Text(value)) => {
                decode_constant_text(value).map_or(ProofTerm::Unknown, ProofTerm::text)
            }
            Expr::Literal(Literal::Unit) => ProofTerm::unit(),
            Expr::Tuple(elements) => ProofTerm::Tuple(
                elements
                    .iter()
                    .map(|element| self.proof_term_inner(*element, depth + 1))
                    .collect(),
            ),
            Expr::Path(_) => self.proof_resolution_term(expression),
            Expr::SelfValue => ProofTerm::Place(ProofPlace {
                root: ProofRoot::SelfValue,
                fields: Vec::new(),
            }),
            Expr::ResultValue => ProofTerm::Place(ProofPlace {
                root: ProofRoot::ResultValue,
                fields: Vec::new(),
            }),
            Expr::Old(value) => self.proof_term_inner(*value, depth + 1),
            Expr::Block { statements, tail } if statements.is_empty() => tail
                .map_or(ProofTerm::unit(), |tail| {
                    self.proof_term_inner(tail, depth + 1)
                }),
            Expr::If {
                condition,
                then_branch,
                else_branch,
            } => match self
                .proof_facts
                .prove(&self.proof_term_inner(*condition, depth + 1))
            {
                ProofResult::Proven => self.proof_term_inner(*then_branch, depth + 1),
                ProofResult::Disproven => else_branch.map_or(ProofTerm::unit(), |branch| {
                    self.proof_term_inner(branch, depth + 1)
                }),
                ProofResult::Unknown => ProofTerm::Unknown,
            },
            Expr::Call { arguments, .. } => {
                let resolution = self.semantics.calls.get(expression);
                match resolution.map(|resolution| &resolution.target) {
                    Some(CallTarget::Function(definition))
                        if Some(*definition) == self.analyzer.canonical_std_items.is_finite
                            && arguments.len() == 1 =>
                    {
                        ProofTerm::is_finite(
                            self.proof_term_inner(arguments[0], depth + 1).unrefine(),
                        )
                    }
                    Some(CallTarget::Builtin(BuiltinValue::Unit)) => ProofTerm::unit(),
                    Some(CallTarget::EnumVariant(variant)) => {
                        let owner = match &self.analyzer.program.definitions[*variant].kind {
                            DefinitionKind::Variant(variant) => variant.owner,
                            _ => return ProofTerm::Unknown,
                        };
                        ProofTerm::Variant {
                            owner,
                            variant: *variant,
                            payload: arguments
                                .iter()
                                .map(|argument| self.proof_term_inner(*argument, depth + 1))
                                .collect(),
                        }
                    }
                    Some(CallTarget::RefinedConstructor(definition))
                        if self.semantics.construction_checks.get(expression)
                            == Some(&ConstructionCheck::Proven)
                            && arguments.len() == 1 =>
                    {
                        ProofTerm::Refined {
                            definition: *definition,
                            value: Box::new(
                                self.proof_term_inner(arguments[0], depth + 1).unrefine(),
                            ),
                        }
                    }
                    _ => ProofTerm::Unknown,
                }
            }
            Expr::Field { receiver, .. } => {
                let Some(place) = self.semantics.expression_places.get(expression) else {
                    return ProofTerm::Unknown;
                };
                let Some(PlaceProjection::Field(field)) = place.projections.last() else {
                    return ProofTerm::Unknown;
                };
                self.proof_term_inner(*receiver, depth + 1).field(*field)
            }
            Expr::Unary { op, operand } => ProofTerm::unary(
                proof_unary(*op),
                self.proof_term_inner(*operand, depth + 1).unrefine(),
            ),
            Expr::Binary { op, left, right } => proof_binary_term(
                *op,
                self.proof_term_inner(*left, depth + 1).unrefine(),
                self.proof_term_inner(*right, depth + 1).unrefine(),
            ),
            Expr::RecordLiteral { ty, fields } => {
                let Some(canonical) = self.semantics.record_fields.get(expression) else {
                    return ProofTerm::Unknown;
                };
                let module = self.analyzer.program.definitions[self.environment.owner].module;
                let definition =
                    crate::Resolver::new(self.analyzer.program, self.analyzer.def_maps, module)
                        .resolve_definition(ty, Namespace::Type)
                        .ok();
                let Some(definition) = definition else {
                    return ProofTerm::Unknown;
                };
                if self.semantics.construction_checks.get(expression)
                    == Some(&ConstructionCheck::Runtime)
                {
                    return ProofTerm::Unknown;
                }
                let _ = fields;
                ProofTerm::Record {
                    definition,
                    fields: canonical
                        .iter()
                        .map(|(field, value)| (*field, self.proof_term_inner(*value, depth + 1)))
                        .collect(),
                }
            }
            Expr::List(_)
            | Expr::Block { .. }
            | Expr::Match { .. }
            | Expr::MethodCall { .. }
            | Expr::QualifiedMethodCall { .. }
            | Expr::Assign { .. }
            | Expr::Await(_)
            | Expr::Propagate(_)
            | Expr::Return(_)
            | Expr::Error => ProofTerm::Unknown,
        };
        if matches!(
            self.semantics.expression_coercions.get(expression),
            Some(Coercion::RefinedToBase { .. })
        ) {
            term = term.unrefine();
        }
        term
    }

    fn proof_resolution_term(&self, expression: ExprId) -> ProofTerm {
        match self
            .semantics
            .expression_resolutions
            .get(expression)
            .copied()
        {
            Some(Resolution::Param(parameter)) => ProofTerm::Place(ProofPlace {
                root: ProofRoot::Param(parameter.raw()),
                fields: Vec::new(),
            }),
            Some(Resolution::Local(local)) => {
                self.local_terms.get(&local).cloned().unwrap_or_else(|| {
                    ProofTerm::Place(ProofPlace {
                        root: ProofRoot::Local(local.raw()),
                        fields: Vec::new(),
                    })
                })
            }
            Some(Resolution::SelfValue) => ProofTerm::Place(ProofPlace {
                root: ProofRoot::SelfValue,
                fields: Vec::new(),
            }),
            Some(Resolution::ResultValue) => ProofTerm::Place(ProofPlace {
                root: ProofRoot::ResultValue,
                fields: Vec::new(),
            }),
            Some(Resolution::Builtin(BuiltinValue::Unit)) => ProofTerm::unit(),
            Some(Resolution::Definition(definition)) => {
                if let Some(value) = self.analyzer.typed.constants.get(definition) {
                    return constant_proof_term(value);
                }
                let DefinitionKind::Variant(variant_definition) =
                    &self.analyzer.program.definitions[definition].kind
                else {
                    return ProofTerm::Unknown;
                };
                ProofTerm::Variant {
                    owner: variant_definition.owner,
                    variant: definition,
                    payload: Vec::new(),
                }
            }
            _ => ProofTerm::Unknown,
        }
    }

    fn contract_proof_term(
        &self,
        body: BodyId,
        self_value: Option<&ProofTerm>,
        arguments: &BTreeMap<ParamId, ProofTerm>,
    ) -> ProofTerm {
        let bindings = BTreeMap::new();
        self.contract_proof_term_inner(
            body,
            self.analyzer.program.bodies[body].root,
            self_value,
            arguments,
            &bindings,
            false,
            0,
        )
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn contract_proof_term_inner(
        &self,
        body: BodyId,
        expression: ExprId,
        self_value: Option<&ProofTerm>,
        arguments: &BTreeMap<ParamId, ProofTerm>,
        bindings: &BTreeMap<LocalId, ProofTerm>,
        old: bool,
        depth: u16,
    ) -> ProofTerm {
        if depth >= 256 {
            return ProofTerm::Unknown;
        }
        let source = &self.analyzer.program.bodies[body];
        let semantics = if body == self.body {
            &self.semantics
        } else {
            let Some(semantics) = self.analyzer.typed.bodies.get(body) else {
                return ProofTerm::Unknown;
            };
            semantics
        };
        match &source.expressions[expression] {
            Expr::Literal(Literal::Bool(value)) => ProofTerm::bool(*value),
            Expr::Literal(Literal::Int(value)) => value
                .parse::<i64>()
                .map_or(ProofTerm::Unknown, ProofTerm::int),
            Expr::Literal(Literal::Float(value)) => value
                .parse::<f64>()
                .map_or(ProofTerm::Unknown, ProofTerm::float),
            Expr::Literal(Literal::Text(value)) => {
                decode_constant_text(value).map_or(ProofTerm::Unknown, ProofTerm::text)
            }
            Expr::Literal(Literal::Unit) => ProofTerm::unit(),
            Expr::Path(_) => match semantics.expression_resolutions.get(expression).copied() {
                Some(Resolution::Param(parameter)) => {
                    arguments.get(&parameter).cloned().unwrap_or_else(|| {
                        ProofTerm::Place(ProofPlace {
                            root: if old {
                                ProofRoot::OldParam(parameter.raw())
                            } else {
                                ProofRoot::Param(parameter.raw())
                            },
                            fields: Vec::new(),
                        })
                    })
                }
                Some(Resolution::Local(local)) => {
                    bindings.get(&local).cloned().unwrap_or(ProofTerm::Unknown)
                }
                Some(Resolution::Builtin(BuiltinValue::Unit)) => ProofTerm::unit(),
                Some(Resolution::Definition(definition)) => {
                    if let Some(value) = self.analyzer.typed.constants.get(definition) {
                        return constant_proof_term(value);
                    }
                    let DefinitionKind::Variant(variant_definition) =
                        &self.analyzer.program.definitions[definition].kind
                    else {
                        return ProofTerm::Unknown;
                    };
                    ProofTerm::Variant {
                        owner: variant_definition.owner,
                        variant: definition,
                        payload: Vec::new(),
                    }
                }
                _ => ProofTerm::Unknown,
            },
            Expr::SelfValue => self_value.cloned().unwrap_or_else(|| {
                ProofTerm::Place(ProofPlace {
                    root: if old {
                        ProofRoot::OldSelf
                    } else {
                        ProofRoot::SelfValue
                    },
                    fields: Vec::new(),
                })
            }),
            Expr::ResultValue => ProofTerm::Place(ProofPlace {
                root: ProofRoot::ResultValue,
                fields: Vec::new(),
            }),
            Expr::Old(value) => self.contract_proof_term_inner(
                body,
                *value,
                self_value,
                arguments,
                bindings,
                true,
                depth + 1,
            ),
            Expr::Field { receiver, .. } => {
                let Some(place) = semantics.expression_places.get(expression) else {
                    return ProofTerm::Unknown;
                };
                let Some(PlaceProjection::Field(field)) = place.projections.last() else {
                    return ProofTerm::Unknown;
                };
                self.contract_proof_term_inner(
                    body,
                    *receiver,
                    self_value,
                    arguments,
                    bindings,
                    old,
                    depth + 1,
                )
                .field(*field)
            }
            Expr::Unary { op, operand } => ProofTerm::unary(
                proof_unary(*op),
                self.contract_proof_term_inner(
                    body,
                    *operand,
                    self_value,
                    arguments,
                    bindings,
                    old,
                    depth + 1,
                )
                .unrefine(),
            ),
            Expr::Binary { op, left, right } => proof_binary_term(
                *op,
                self.contract_proof_term_inner(
                    body,
                    *left,
                    self_value,
                    arguments,
                    bindings,
                    old,
                    depth + 1,
                )
                .unrefine(),
                self.contract_proof_term_inner(
                    body,
                    *right,
                    self_value,
                    arguments,
                    bindings,
                    old,
                    depth + 1,
                )
                .unrefine(),
            ),
            Expr::Call {
                arguments: values, ..
            } if semantics.calls.get(expression).is_some_and(|resolution| {
                matches!(
                    resolution.target,
                    CallTarget::Function(definition)
                        if Some(definition) == self.analyzer.canonical_std_items.is_finite
                )
            }) && values.len() == 1 =>
            {
                ProofTerm::is_finite(
                    self.contract_proof_term_inner(
                        body,
                        values[0],
                        self_value,
                        arguments,
                        bindings,
                        old,
                        depth + 1,
                    )
                    .unrefine(),
                )
            }
            Expr::Block { statements, tail } if statements.is_empty() => {
                tail.map_or(ProofTerm::unit(), |tail| {
                    self.contract_proof_term_inner(
                        body,
                        tail,
                        self_value,
                        arguments,
                        bindings,
                        old,
                        depth + 1,
                    )
                })
            }
            // Exhaustive match remains runtime-checked unless its complete
            // scrutinee/pattern proof can be represented by a later domain.
            Expr::Error
            | Expr::Tuple(_)
            | Expr::List(_)
            | Expr::If { .. }
            | Expr::Match { .. }
            | Expr::Call { .. }
            | Expr::MethodCall { .. }
            | Expr::QualifiedMethodCall { .. }
            | Expr::Assign { .. }
            | Expr::RecordLiteral { .. }
            | Expr::Await(_)
            | Expr::Propagate(_)
            | Expr::Return(_)
            | Expr::Block { .. } => ProofTerm::Unknown,
        }
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
        if path.segments.len() == 1 && matches!(path.segments[0].name.as_str(), "List" | "TextMap")
        {
            if !arguments.is_empty() {
                self.call_arity(expression, 0, arguments.len());
            }
            let explicit = self.resolve_call_type_arguments(type_arguments);
            let value = if explicit.len() == 1 {
                explicit[0]
            } else {
                let constructor = path.segments[0].name.as_str();
                self.error_at(
                    "TypeMismatch",
                    format!(
                        "{constructor} construction requires exactly one value type: `{constructor}[T]()`"
                    ),
                    expression,
                );
                self.types().error()
            };
            let (target, result) = if path.segments[0].name.as_str() == "List" {
                (
                    BuiltinValue::ListNew,
                    self.types().intern(TyData::List(value)),
                )
            } else {
                (
                    BuiltinValue::TextMapNew,
                    self.types().intern(TyData::TextMap(value)),
                )
            };
            self.semantics.calls.insert(
                expression,
                CallResolution {
                    target: CallTarget::Builtin(target),
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
            let builtin = self
                .compiler_std_primitive_call(&path)
                .map(|primitive| match primitive {
                    crate::std_primitives::CompilerStdPrimitive::FloatFromInt => {
                        BuiltinValue::IntToFloat
                    }
                    crate::std_primitives::CompilerStdPrimitive::FloatFormat => {
                        BuiltinValue::FloatFormat
                    }
                    crate::std_primitives::CompilerStdPrimitive::FloatIsFinite => {
                        BuiltinValue::FloatIsFinite
                    }
                    crate::std_primitives::CompilerStdPrimitive::FloatParseStatus => {
                        BuiltinValue::FloatParseStatus
                    }
                    crate::std_primitives::CompilerStdPrimitive::FloatToInt => {
                        BuiltinValue::FloatToIntStatus
                    }
                    crate::std_primitives::CompilerStdPrimitive::DurationMilliseconds => {
                        BuiltinValue::DurationMilliseconds
                    }
                    crate::std_primitives::CompilerStdPrimitive::FileOpenRead => {
                        BuiltinValue::FileOpenRead
                    }
                    crate::std_primitives::CompilerStdPrimitive::FileCreate => {
                        BuiltinValue::FileCreate
                    }
                    crate::std_primitives::CompilerStdPrimitive::FileTryOpenRead => {
                        BuiltinValue::FileTryOpenRead
                    }
                    crate::std_primitives::CompilerStdPrimitive::FileTryCreate => {
                        BuiltinValue::FileTryCreate
                    }
                    crate::std_primitives::CompilerStdPrimitive::FileReadText => {
                        BuiltinValue::FileReadText
                    }
                    crate::std_primitives::CompilerStdPrimitive::FileWriteText => {
                        BuiltinValue::FileWriteText
                    }
                    crate::std_primitives::CompilerStdPrimitive::FileTryReadText => {
                        BuiltinValue::FileTryReadText
                    }
                    crate::std_primitives::CompilerStdPrimitive::FileTryWriteText => {
                        BuiltinValue::FileTryWriteText
                    }
                    crate::std_primitives::CompilerStdPrimitive::FileClose => {
                        BuiltinValue::FileClose
                    }
                    crate::std_primitives::CompilerStdPrimitive::IoErrorKind => {
                        BuiltinValue::IoErrorKind
                    }
                    crate::std_primitives::CompilerStdPrimitive::IoErrorMessage => {
                        BuiltinValue::IoErrorMessage
                    }
                    crate::std_primitives::CompilerStdPrimitive::IoWriteStdout => {
                        BuiltinValue::StdoutWrite
                    }
                    crate::std_primitives::CompilerStdPrimitive::LogWrite => BuiltinValue::LogWrite,
                    crate::std_primitives::CompilerStdPrimitive::ProcessArgumentCount => {
                        BuiltinValue::ProcessArgumentCount
                    }
                    crate::std_primitives::CompilerStdPrimitive::ProcessArgumentAt => {
                        BuiltinValue::ProcessArgumentAt
                    }
                    crate::std_primitives::CompilerStdPrimitive::ProcessEnvironment => {
                        BuiltinValue::ProcessEnvironment
                    }
                    crate::std_primitives::CompilerStdPrimitive::SocketConnect => {
                        BuiltinValue::SocketConnect
                    }
                    crate::std_primitives::CompilerStdPrimitive::SocketTryConnect => {
                        BuiltinValue::SocketTryConnect
                    }
                    crate::std_primitives::CompilerStdPrimitive::SocketReadText => {
                        BuiltinValue::SocketReadText
                    }
                    crate::std_primitives::CompilerStdPrimitive::SocketWriteText => {
                        BuiltinValue::SocketWriteText
                    }
                    crate::std_primitives::CompilerStdPrimitive::SocketTryReadText => {
                        BuiltinValue::SocketTryReadText
                    }
                    crate::std_primitives::CompilerStdPrimitive::SocketTryWriteText => {
                        BuiltinValue::SocketTryWriteText
                    }
                    crate::std_primitives::CompilerStdPrimitive::SocketClose => {
                        BuiltinValue::SocketClose
                    }
                })
                .or_else(|| match path.segments[0].name.as_str() {
                    "Some" => Some(BuiltinValue::Some),
                    "Ok" => Some(BuiltinValue::Ok),
                    "Err" => Some(BuiltinValue::Err),
                    _ => None,
                });
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
            let proof = self.refinement_proof(definition, arguments[0]);
            let carries_task = self.has_task_obligation(base, &mut BTreeSet::new(), 0);
            let nominal = self.types().intern(TyData::Nominal {
                definition,
                arguments: Vec::new(),
            });
            let check = if proof == ProofResult::Proven {
                ConstructionCheck::Proven
            } else {
                ConstructionCheck::Runtime
            };
            self.semantics.construction_checks.insert(expression, check);
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
            return match proof {
                ProofResult::Proven => nominal,
                ProofResult::Disproven => {
                    if carries_task {
                        self.consume_task_obligation(arguments[0]);
                    }
                    let name = self.analyzer.program.definitions[definition]
                        .name
                        .as_ref()
                        .map_or("<constrained type>", Name::as_str);
                    self.error_at(
                        "ConstraintUnsatisfied",
                        format!("value is statically known to violate `{name}`"),
                        expression,
                    );
                    self.types().error()
                }
                ProofResult::Unknown => {
                    if carries_task {
                        self.error_at(
                            "TaskFallibleConstructionUnsupported",
                            "a Task-carrying constrained value requires a statically proven predicate because a failed runtime check cannot discard its Task",
                            expression,
                        );
                        self.consume_task_obligation(arguments[0]);
                        return self.types().error();
                    }
                    let constraint_error = self.types().builtin(BuiltinType::ConstraintError);
                    self.types().intern(TyData::Result {
                        ok: nominal,
                        error: constraint_error,
                    })
                }
            };
        }
        self.error_at(
            "UnknownName",
            format!("unknown callable `{}`", path.as_string()),
            expression,
        );
        self.types().error()
    }

    fn finish_async_call(&mut self, expression: ExprId, is_async: bool, output: TyId) -> TyId {
        if !is_async {
            return output;
        }
        if self.has_task_obligation(output, &mut BTreeSet::new(), 0) {
            self.error_at(
                "TaskAsyncResultUnsupported",
                "an instantiated async callable cannot complete with a Task-carrying result before runtime reparenting is available",
                expression,
            );
        }
        self.types().intern(TyData::Task(output))
    }

    #[allow(clippy::too_many_lines)]
    fn check_builtin_call(
        &mut self,
        expression: ExprId,
        builtin: BuiltinValue,
        arguments: &[ExprId],
        expected: Option<TyId>,
    ) -> TyId {
        let mut receiver = None;
        let result = match builtin {
            BuiltinValue::Some => self.check_some_call(expression, arguments, expected),
            BuiltinValue::Ok | BuiltinValue::Err => {
                self.check_result_constructor_call(expression, builtin, arguments, expected)
            }
            BuiltinValue::FloatParseStatus
            | BuiltinValue::FloatFormat
            | BuiltinValue::FloatIsFinite
            | BuiltinValue::IntToFloat
            | BuiltinValue::FloatToIntStatus
            | BuiltinValue::ProcessArgumentCount
            | BuiltinValue::ProcessArgumentAt
            | BuiltinValue::ProcessEnvironment
            | BuiltinValue::DurationMilliseconds
            | BuiltinValue::FileOpenRead
            | BuiltinValue::FileCreate
            | BuiltinValue::FileTryOpenRead
            | BuiltinValue::FileTryCreate
            | BuiltinValue::IoErrorKind
            | BuiltinValue::IoErrorMessage
            | BuiltinValue::SocketConnect
            | BuiltinValue::SocketTryConnect
            | BuiltinValue::LogWrite
            | BuiltinValue::StdoutWrite => {
                self.check_builtin_function_call(expression, builtin, arguments)
            }
            BuiltinValue::FileReadText
            | BuiltinValue::FileWriteText
            | BuiltinValue::FileTryReadText
            | BuiltinValue::FileTryWriteText
            | BuiltinValue::FileClose
            | BuiltinValue::SocketReadText
            | BuiltinValue::SocketWriteText
            | BuiltinValue::SocketTryReadText
            | BuiltinValue::SocketTryWriteText
            | BuiltinValue::SocketClose => {
                let (result, passing) =
                    self.check_resource_primitive_call(expression, builtin, arguments);
                receiver = Some(passing);
                result
            }
            BuiltinValue::ListNew
            | BuiltinValue::TextMapNew
            | BuiltinValue::ListAdd
            | BuiltinValue::ListLength
            | BuiltinValue::ListGet
            | BuiltinValue::TextMapLength
            | BuiltinValue::TextMapContains
            | BuiltinValue::TextMapGet
            | BuiltinValue::TextMapEntryAt
            | BuiltinValue::TextMapInsert
            | BuiltinValue::ListToTextMap
            | BuiltinValue::TextMapRemove
            | BuiltinValue::TaskFaultCode
            | BuiltinValue::TaskFaultMessage
            | BuiltinValue::DurationAsMilliseconds
            | BuiltinValue::TextLength
            | BuiltinValue::TextGet
            | BuiltinValue::TextConcat
            | BuiltinValue::TextContains
            | BuiltinValue::TextEncodeUtf8
            | BuiltinValue::BytesLength
            | BuiltinValue::BytesGet
            | BuiltinValue::BytesAdd
            | BuiltinValue::BytesAppend
            | BuiltinValue::BytesDecodeUtf8
            | BuiltinValue::PathFromText
            | BuiltinValue::PathAsText
            | BuiltinValue::PathJoin => {
                self.error_at(
                    "TypeMismatch",
                    "List builtin is only available through List construction or methods",
                    expression,
                );
                self.types().error()
            }
            BuiltinValue::Unit
            | BuiltinValue::None
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
                receiver,
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

    #[allow(clippy::too_many_lines)]
    fn check_builtin_function_call(
        &mut self,
        expression: ExprId,
        builtin: BuiltinValue,
        arguments: &[ExprId],
    ) -> TyId {
        match builtin {
            BuiltinValue::FloatParseStatus => {
                let text = self.types().builtin(BuiltinType::Text);
                self.check_fixed_arguments(expression, arguments, &[text]);
                let float = self.types().builtin(BuiltinType::Float);
                let status = self.types().builtin(BuiltinType::Int);
                self.types().intern(TyData::Tuple(vec![float, status]))
            }
            BuiltinValue::FloatFormat => {
                let float = self.types().builtin(BuiltinType::Float);
                self.check_fixed_arguments(expression, arguments, &[float]);
                self.types().builtin(BuiltinType::Text)
            }
            BuiltinValue::FloatIsFinite => {
                let float = self.types().builtin(BuiltinType::Float);
                self.check_fixed_arguments(expression, arguments, &[float]);
                self.types().builtin(BuiltinType::Bool)
            }
            BuiltinValue::IntToFloat => {
                let int = self.types().builtin(BuiltinType::Int);
                self.check_fixed_arguments(expression, arguments, &[int]);
                self.types().builtin(BuiltinType::Float)
            }
            BuiltinValue::FloatToIntStatus => {
                let float = self.types().builtin(BuiltinType::Float);
                self.check_fixed_arguments(expression, arguments, &[float]);
                let int = self.types().builtin(BuiltinType::Int);
                self.types().intern(TyData::Tuple(vec![int, int]))
            }
            BuiltinValue::ProcessArgumentCount => {
                self.check_fixed_arguments(expression, arguments, &[]);
                self.types().builtin(BuiltinType::Int)
            }
            BuiltinValue::ProcessArgumentAt => {
                let int = self.types().builtin(BuiltinType::Int);
                self.check_fixed_arguments(expression, arguments, &[int]);
                self.types().builtin(BuiltinType::Text)
            }
            BuiltinValue::ProcessEnvironment => {
                let text = self.types().builtin(BuiltinType::Text);
                self.check_fixed_arguments(expression, arguments, &[text]);
                self.types().intern(TyData::Option(text))
            }
            BuiltinValue::DurationMilliseconds => {
                let int = self.types().builtin(BuiltinType::Int);
                self.check_fixed_arguments(expression, arguments, &[int]);
                self.types().builtin(BuiltinType::Duration)
            }
            BuiltinValue::FileOpenRead | BuiltinValue::FileCreate => {
                let text = self.types().builtin(BuiltinType::Text);
                self.check_fixed_arguments(expression, arguments, &[text]);
                let file = self.canonical_file_type(expression);
                self.types().intern(TyData::Task(file))
            }
            BuiltinValue::FileTryOpenRead | BuiltinValue::FileTryCreate => {
                let text = self.types().builtin(BuiltinType::Text);
                self.check_fixed_arguments(expression, arguments, &[text]);
                let file = self.canonical_file_type(expression);
                self.try_resource_task(file, expression)
            }
            BuiltinValue::IoErrorKind => {
                let error = self.canonical_io_error_type(expression);
                self.check_fixed_arguments(expression, arguments, &[error]);
                let definition = self.analyzer.canonical_std_items.io_error_kind;
                self.canonical_std_type(definition, "std.io.IoErrorKind", expression)
            }
            BuiltinValue::IoErrorMessage => {
                let error = self.canonical_io_error_type(expression);
                self.check_fixed_arguments(expression, arguments, &[error]);
                self.types().builtin(BuiltinType::Text)
            }
            BuiltinValue::SocketConnect => {
                let text = self.types().builtin(BuiltinType::Text);
                let int = self.types().builtin(BuiltinType::Int);
                self.check_fixed_arguments(expression, arguments, &[text, int]);
                let socket = self.canonical_socket_type(expression);
                self.types().intern(TyData::Task(socket))
            }
            BuiltinValue::SocketTryConnect => {
                let text = self.types().builtin(BuiltinType::Text);
                let int = self.types().builtin(BuiltinType::Int);
                self.check_fixed_arguments(expression, arguments, &[text, int]);
                let socket = self.canonical_socket_type(expression);
                self.try_resource_task(socket, expression)
            }
            BuiltinValue::LogWrite => {
                let definition = self.analyzer.canonical_std_items.log_level;
                let level = self.canonical_std_type(definition, "std.log.LogLevel", expression);
                let text = self.types().builtin(BuiltinType::Text);
                let fields = self.types().intern(TyData::TextMap(text));
                self.check_fixed_arguments(expression, arguments, &[level, text, fields]);
                self.types().builtin(BuiltinType::Unit)
            }
            BuiltinValue::StdoutWrite => {
                let text = self.types().builtin(BuiltinType::Text);
                self.check_fixed_arguments(expression, arguments, &[text]);
                self.types().builtin(BuiltinType::Unit)
            }
            _ => unreachable!("caller filters standard builtins"),
        }
    }

    fn check_resource_primitive_call(
        &mut self,
        expression: ExprId,
        builtin: BuiltinValue,
        arguments: &[ExprId],
    ) -> (TyId, ReceiverPassing) {
        let file = matches!(
            builtin,
            BuiltinValue::FileReadText
                | BuiltinValue::FileWriteText
                | BuiltinValue::FileTryReadText
                | BuiltinValue::FileTryWriteText
                | BuiltinValue::FileClose
        );
        let resource = if file {
            self.canonical_file_type(expression)
        } else {
            self.canonical_socket_type(expression)
        };
        let text = self.types().builtin(BuiltinType::Text);
        let unit = self.types().builtin(BuiltinType::Unit);
        let passing = if matches!(builtin, BuiltinValue::FileClose | BuiltinValue::SocketClose) {
            ReceiverPassing::InOut
        } else {
            ReceiverPassing::Value
        };
        let (parameters, result) = match builtin {
            BuiltinValue::FileReadText | BuiltinValue::SocketReadText => {
                (Vec::new(), self.types().intern(TyData::Task(text)))
            }
            BuiltinValue::FileWriteText | BuiltinValue::SocketWriteText => {
                (vec![text], self.types().intern(TyData::Task(unit)))
            }
            BuiltinValue::FileTryReadText | BuiltinValue::SocketTryReadText => {
                let error = self.canonical_io_error_type(expression);
                let result = self.types().intern(TyData::Result { ok: text, error });
                (Vec::new(), self.types().intern(TyData::Task(result)))
            }
            BuiltinValue::FileTryWriteText | BuiltinValue::SocketTryWriteText => {
                let error = self.canonical_io_error_type(expression);
                let result = self.types().intern(TyData::Result { ok: unit, error });
                (vec![text], self.types().intern(TyData::Task(result)))
            }
            BuiltinValue::FileClose | BuiltinValue::SocketClose => (Vec::new(), unit),
            _ => unreachable!("caller filters private resource primitives"),
        };
        let Some((receiver, call_arguments)) = arguments.split_first() else {
            self.call_arity(expression, parameters.len() + 1, 0);
            return (self.types().error(), passing);
        };
        self.check_call_receiver(*receiver, Some(resource));
        if passing == ReceiverPassing::InOut
            && self
                .semantics
                .expression_places
                .get(*receiver)
                .is_none_or(|place| place.mutability != Mutability::Mutable)
        {
            self.error_at(
                "MutReceiverRequiresVar",
                "the compiler-private close primitive requires a mutable self receiver",
                *receiver,
            );
        }
        self.with_inout_argument_scope(*receiver, passing == ReceiverPassing::InOut, |checker| {
            checker.check_fixed_arguments(expression, call_arguments, &parameters);
        });
        (result, passing)
    }

    fn try_resource_task(&mut self, resource: TyId, expression: ExprId) -> TyId {
        let error = self.canonical_io_error_type(expression);
        let result = self.types().intern(TyData::Result {
            ok: resource,
            error,
        });
        self.types().intern(TyData::Task(result))
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
            if self.has_must_scope_obligation_root(*ty) {
                self.error_at(
                    "MustScopeArgumentNotAllowed",
                    "a value containing a MustScope resource cannot be passed as an ordinary argument",
                    *argument,
                );
            }
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
        let dynamic_mutations =
            self.coerce_callable_arguments(signature, actual_types, &substitution);
        self.apply_mutable_dynamic_mutations(dynamic_mutations);
        let return_ty = self.types().substitute(signature.return_ty, &substitution);
        (return_ty, substitution)
    }

    fn coerce_callable_arguments(
        &mut self,
        signature: &CallableSignature,
        actual_types: Vec<(ExprId, TyId, TyId)>,
        substitution: &Substitution,
    ) -> Vec<(ExprId, Place, TyId)> {
        let mut dynamic_mutations = Vec::new();
        for (argument, actual, parameter_ty) in actual_types {
            let expected = self.types().substitute(parameter_ty, substitution);
            if self.has_must_scope_obligation_root(expected) {
                self.error_at(
                    "MustScopeArgumentNotAllowed",
                    "a value containing a MustScope resource cannot be passed as an ordinary argument",
                    argument,
                );
            }
            let actual_has_task = self.has_task_obligation(actual, &mut BTreeSet::new(), 0);
            if actual_has_task {
                let declared_has_task =
                    self.has_task_obligation(parameter_ty, &mut BTreeSet::new(), 0);
                if signature.is_async {
                    self.error_at(
                        "TaskAsyncTransferUnsupported",
                        "a Task-carrying value cannot cross an async call boundary before runtime reparenting is available",
                        argument,
                    );
                } else if !declared_has_task {
                    self.error_at(
                        "TaskGenericTransferUnsupported",
                        "a Task-carrying value cannot pass through an unconstrained generic parameter",
                        argument,
                    );
                }
                self.consume_task_obligation(argument);
            }
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
            if let Some(view) = self.semantics.views.get(argument)
                && view.mutable
            {
                let owner = match &view.source {
                    ViewSource::Concrete {
                        writeback: Some(owner),
                        ..
                    }
                    | ViewSource::Interface { owner } => Some(owner.clone()),
                    ViewSource::Concrete {
                        writeback: None, ..
                    } => None,
                };
                if let Some(owner) = owner {
                    dynamic_mutations.push((argument, owner, actual));
                }
            }
        }
        dynamic_mutations
    }

    fn apply_mutable_dynamic_mutations(&mut self, dynamic_mutations: Vec<(ExprId, Place, TyId)>) {
        let dirty_before_call = self.self_dirty;
        let mut dirties_self = false;
        let mut reported_isolation = false;
        for (argument, owner, owner_ty) in dynamic_mutations {
            let crosses_self = self.check_invariant_mutation_boundary(
                argument,
                &owner,
                InvariantMutation::ReceiverCall,
            );
            if crosses_self && dirty_before_call && !reported_isolation {
                self.error_at(
                    "InvariantIsolationViolation",
                    "a mutated receiver cannot enter another mutable interface call before its invariant is restored",
                    argument,
                );
                reported_isolation = true;
            }
            self.invalidate_mutated_place(&owner, owner_ty);
            dirties_self |= crosses_self;
        }
        self.self_dirty |= dirties_self;
    }

    fn transfer_task_receiver(
        &mut self,
        receiver: ExprId,
        receiver_ty: TyId,
        declared_task_carrier: bool,
        is_async: bool,
    ) {
        if !self.has_task_obligation(receiver_ty, &mut BTreeSet::new(), 0) {
            return;
        }
        if is_async {
            self.error_at(
                "TaskAsyncTransferUnsupported",
                "a Task-carrying receiver cannot cross an async call boundary before runtime reparenting is available",
                receiver,
            );
        } else if !declared_task_carrier {
            self.error_at(
                "TaskGenericTransferUnsupported",
                "a Task-carrying receiver cannot pass through a generic or concept receiver that does not explicitly declare the obligation",
                receiver,
            );
        }
        self.consume_task_obligation(receiver);
    }

    fn witness_declares_task_receiver(&mut self, witness: &WitnessSelection) -> bool {
        let WitnessSource::Implementation(implementation) = witness.source else {
            return false;
        };
        let Some(target) = self
            .analyzer
            .impl_index
            .header(implementation)
            .map(|header| header.target)
        else {
            return false;
        };
        self.has_task_obligation(target, &mut BTreeSet::new(), 0)
    }

    fn value_path_lookup(&self, path: &Path) -> ValuePathLookup {
        let module = self.analyzer.program.definitions[self.environment.owner].module;
        if let [segment] = path.segments.as_slice()
            && (self
                .scopes
                .iter()
                .rev()
                .any(|scope| scope.contains_key(&segment.name))
                || self.environment.params.contains_key(&segment.name))
        {
            return ValuePathLookup::Bound;
        }
        match crate::Resolver::new(self.analyzer.program, self.analyzer.def_maps, module)
            .resolve_definition(path, Namespace::Value)
        {
            Ok(_) => ValuePathLookup::Bound,
            Err(ResolveError::Missing) => ValuePathLookup::Missing,
            Err(
                ResolveError::Duplicate(_)
                | ResolveError::Private(_)
                | ResolveError::UnknownModule(_),
            ) => ValuePathLookup::Invalid,
        }
    }

    fn resolves_task_intrinsic_namespace(&self, path: &Path) -> bool {
        if !crate::task_intrinsics::is_task_namespace(path) {
            return false;
        }
        let module = self.analyzer.program.definitions[self.environment.owner].module;
        if self.value_path_lookup(path) != ValuePathLookup::Missing {
            return false;
        }
        let [segment] = path.segments.as_slice() else {
            return false;
        };
        if self.analyzer.program.modules[module]
            .imports
            .iter()
            .any(|import| {
                import.file == segment.span.file
                    && crate::module_graph::imported_name(import) == Some(&segment.name)
            })
        {
            return false;
        }
        let mut resolver =
            crate::Resolver::new(self.analyzer.program, self.analyzer.def_maps, module);
        for parameter in self.analyzer.generic_ids_for(self.environment.owner) {
            resolver.add_generic_param(
                self.analyzer.program.generic_params[parameter].name.clone(),
                parameter,
            );
        }
        matches!(resolver.resolve_type(path), Err(ResolveError::Missing))
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
            && self.value_path_lookup(&type_path) == ValuePathLookup::Missing
        {
            if let Some((variant, owner)) = self.resolve_qualified_variant(&type_path, method_name)
            {
                return self.check_variant_constructor(
                    expression,
                    variant,
                    owner,
                    Some(type_arguments),
                    arguments,
                    expected,
                );
            }
            if let Some(result) = self.check_compiler_static_method_call(
                expression,
                &type_path,
                method_name,
                type_arguments,
                arguments,
            ) {
                return result;
            }
        }
        let receiver_ty = self.check_call_receiver(receiver, None);
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
        if let TyData::TextMap(value) = self.types().data(receiver_ty).clone()
            && let Some(result) = self.check_text_map_method_call(
                expression,
                receiver,
                value,
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
            && let Some(result) = self.check_builtin_value_method_call(
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
        let scoped_receiver = self
            .semantics
            .expression_places
            .get(receiver)
            .and_then(|place| match place.root {
                PlaceRoot::Local(local) if place.projections.is_empty() => Some(local),
                _ => None,
            })
            .is_some_and(|local| self.scoped_locals.contains(&local));
        if self.is_canonical_resource_type(receiver_ty) && method_name.as_str() == "close" {
            if !type_arguments.is_empty() {
                self.error_at(
                    "TypeMismatch",
                    "resource cleanup does not accept explicit type arguments",
                    expression,
                );
            }
            self.check_fixed_arguments(expression, arguments, &[]);
            self.finish_call_arguments(arguments);
            if scoped_receiver {
                self.error_at(
                    "ManualDisposeOfScopedValue",
                    "a scoped resource is disposed automatically and cannot be closed manually",
                    receiver,
                );
            } else {
                self.error_at(
                    "MustScopeRequiresScoped",
                    "methods on a MustScope value require a receiver already bound with `scoped`",
                    receiver,
                );
            }
            // `close` is deliberately diagnostic-only. Resource cleanup lowers
            // through the canonical Dispose witness, so no executable call
            // target may be recorded for this rejected spelling.
            return self.types().builtin(BuiltinType::Unit);
        }
        if self.has_must_scope_obligation_root(receiver_ty) && !scoped_receiver {
            self.error_at(
                "MustScopeRequiresScoped",
                "methods on a MustScope value require a receiver already bound with `scoped`",
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
        let current_module = self.analyzer.program.definitions[self.environment.owner].module;
        for (implementation, definition) in self.analyzer.program.definitions.iter() {
            let DefinitionKind::InherentImpl(inherent) = &definition.kind else {
                continue;
            };
            if self.analyzer.program.is_test_companion(definition.module)
                && definition.module != current_module
            {
                continue;
            }
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
                let method_definition = &self.analyzer.program.definitions[*method];
                if method_definition.name.as_ref() == Some(method_name)
                    && (method_definition.visibility == Visibility::Public
                        || self
                            .analyzer
                            .program
                            .can_access_private(current_module, method_definition.module))
                {
                    candidates.push((implementation, *method, target, substitution.clone()));
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
            self.reject_explicit_dispose_call(requirement, receiver);
            let Some(signature) =
                self.instantiate_concept_signature(requirement, receiver_ty, &concept)
            else {
                return self.types().error();
            };
            let declared_task_carrier = self.witness_declares_task_receiver(&witness);
            self.transfer_task_receiver(
                receiver,
                receiver_ty,
                declared_task_carrier,
                signature.is_async,
            );
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
            let (return_ty, substitution) = self.with_inout_argument_scope(
                receiver,
                signature.receiver == Some(ReceiverKind::Mutable),
                |checker| {
                    checker.check_callable_arguments(
                        expression,
                        &signature,
                        arguments,
                        &explicit,
                        expected,
                        Substitution::default(),
                    )
                },
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
        let (_, method, declared_receiver_ty, initial) = candidates.pop().expect("one candidate");
        let Some(Signature::Callable(signature)) =
            self.analyzer.typed.signatures.get(method).cloned()
        else {
            return self.types().error();
        };
        let declared_task_carrier =
            self.has_task_obligation(declared_receiver_ty, &mut BTreeSet::new(), 0);
        self.transfer_task_receiver(
            receiver,
            receiver_ty,
            declared_task_carrier,
            signature.is_async,
        );
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
        let (return_ty, substitution) = self.with_inout_argument_scope(
            receiver,
            signature.receiver == Some(ReceiverKind::Mutable),
            |checker| {
                checker.check_callable_arguments(
                    expression, &signature, arguments, &explicit, expected, initial,
                )
            },
        );
        self.finish_call_arguments(arguments);
        let witnesses = self.resolve_bound_witnesses(&signature, &substitution, expression);
        let return_ty = self.normalize_call_type(return_ty, &signature, &substitution, &witnesses);
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

    #[expect(
        clippy::too_many_lines,
        reason = "one receiver dispatcher keeps List.add, List reads, and the exact generic bulk TextMap result inference together"
    )]
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
                self.require_live_task_owner(receiver);
                if arguments.len() != 1 {
                    self.call_arity(expression, 1, arguments.len());
                }
                self.with_inout_argument_scope(receiver, true, |checker| {
                    if let Some(argument) = arguments.first() {
                        checker.check_expr(
                            *argument,
                            Some(element),
                            ExpressionContext::Value,
                        );
                        if checker.has_must_scope_obligation_root(element) {
                            checker.error_at(
                                "MustScopeArgumentNotAllowed",
                                "a value containing a MustScope resource cannot be stored in a List",
                                *argument,
                            );
                        }
                        if checker.has_task_obligation(element, &mut BTreeSet::new(), 0) {
                            checker.consume_task_obligation(*argument);
                        }
                    }
                });
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
                if self.has_task_obligation(element, &mut BTreeSet::new(), 0) {
                    self.error_at(
                        "TaskContainerExtractionUnsupported",
                        "List.get cannot extract a Task-carrying element before container ownership transfer is available",
                        receiver,
                    );
                    self.consume_task_obligation(receiver);
                    let result = self.types().error();
                    (BuiltinValue::ListGet, ReceiverPassing::Value, result)
                } else {
                    let result = self.types().intern(TyData::Option(element));
                    (BuiltinValue::ListGet, ReceiverPassing::Value, result)
                }
            }
            "to_text_map" => {
                if !arguments.is_empty() {
                    self.call_arity(expression, 0, arguments.len());
                }
                let result = match self.types().data(element).clone() {
                    TyData::Tuple(elements) if matches!(elements.as_slice(), [key, _] if *key == self.types().builtin(BuiltinType::Text)) =>
                    {
                        let value = elements[1];
                        if self.has_task_obligation(value, &mut BTreeSet::new(), 0) {
                            self.error_at(
                                "TaskContainerExtractionUnsupported",
                                "List[(Text, V)].to_text_map cannot transfer Task-carrying values before container ownership transfer is available",
                                receiver,
                            );
                            self.types().error()
                        } else {
                            let map = self.types().intern(TyData::TextMap(value));
                            let text = self.types().builtin(BuiltinType::Text);
                            self.types().intern(TyData::Result {
                                ok: map,
                                error: text,
                            })
                        }
                    }
                    _ => {
                        self.error_at(
                            "TypeMismatch",
                            "to_text_map requires a List[(Text, V)] receiver",
                            receiver,
                        );
                        self.types().error()
                    }
                };
                (BuiltinValue::ListToTextMap, ReceiverPassing::Value, result)
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

    fn check_text_map_method_call(
        &mut self,
        expression: ExprId,
        receiver: ExprId,
        value: TyId,
        method_name: &Name,
        type_arguments: &[TypeRefId],
        arguments: &[ExprId],
    ) -> Option<TyId> {
        let text = self.types().builtin(BuiltinType::Text);
        let carries_task = self.has_task_obligation(value, &mut BTreeSet::new(), 0);
        let (builtin, parameters, result) = match method_name.as_str() {
            "length" => (
                BuiltinValue::TextMapLength,
                Vec::new(),
                self.types().builtin(BuiltinType::Int),
            ),
            "contains" => (
                BuiltinValue::TextMapContains,
                vec![text],
                self.types().builtin(BuiltinType::Bool),
            ),
            "get" => (
                BuiltinValue::TextMapGet,
                vec![text],
                self.types().intern(TyData::Option(value)),
            ),
            "entry_at" => {
                let int = self.types().builtin(BuiltinType::Int);
                let entry = self.types().intern(TyData::Tuple(vec![text, value]));
                (
                    BuiltinValue::TextMapEntryAt,
                    vec![int],
                    self.types().intern(TyData::Option(entry)),
                )
            }
            "insert" => (
                BuiltinValue::TextMapInsert,
                vec![text, value],
                self.types().intern(TyData::TextMap(value)),
            ),
            "remove" => (
                BuiltinValue::TextMapRemove,
                vec![text],
                self.types().intern(TyData::TextMap(value)),
            ),
            _ => return None,
        };
        if !type_arguments.is_empty() {
            self.error_at(
                "TypeMismatch",
                "TextMap methods do not accept explicit type arguments",
                expression,
            );
        }
        self.check_fixed_arguments(expression, arguments, &parameters);
        let result = if carries_task
            && matches!(
                method_name.as_str(),
                "get" | "entry_at" | "insert" | "remove"
            ) {
            self.error_at(
                "TaskContainerExtractionUnsupported",
                "this TextMap operation cannot transfer Task-carrying values before container ownership transfer is available",
                expression,
            );
            self.consume_task_obligation(receiver);
            for argument in arguments {
                self.consume_task_obligation(*argument);
            }
            self.types().error()
        } else {
            result
        };
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

    fn check_task_intrinsic_call(
        &mut self,
        expression: ExprId,
        type_path: &Path,
        method_name: &Name,
        type_arguments: &[TypeRefId],
        arguments: &[ExprId],
    ) -> Option<TyId> {
        let intrinsic = self
            .resolves_task_intrinsic_namespace(type_path)
            .then(|| crate::task_intrinsics::resolve(method_name))
            .flatten();
        if let Some(intrinsic) = intrinsic {
            if !type_arguments.is_empty() {
                self.error_at(
                    "TypeMismatch",
                    "Task standard-library functions do not accept explicit type arguments",
                    expression,
                );
            }
            let result = match intrinsic {
                TaskIntrinsic::Sleep => self.check_sleep(expression, arguments),
                TaskIntrinsic::All => {
                    self.check_task_join(expression, TaskJoinPolicy::All, arguments)
                }
                TaskIntrinsic::Settled => {
                    self.check_task_join(expression, TaskJoinPolicy::Settled, arguments)
                }
                TaskIntrinsic::Any => {
                    self.check_task_join(expression, TaskJoinPolicy::Any, arguments)
                }
                TaskIntrinsic::Race => {
                    self.check_task_join(expression, TaskJoinPolicy::Race, arguments)
                }
            };
            self.semantics.calls.insert(
                expression,
                CallResolution {
                    target: CallTarget::TaskIntrinsic(intrinsic),
                    substitution: Substitution::default(),
                    dispatch_witness: None,
                    witnesses: Vec::new(),
                    receiver: None,
                },
            );
            return Some(result);
        }
        None
    }

    fn check_compiler_static_method_call(
        &mut self,
        expression: ExprId,
        type_path: &Path,
        method_name: &Name,
        type_arguments: &[TypeRefId],
        arguments: &[ExprId],
    ) -> Option<TyId> {
        if let Some(result) = self.check_task_intrinsic_call(
            expression,
            type_path,
            method_name,
            type_arguments,
            arguments,
        ) {
            return Some(result);
        }
        let [segment] = type_path.segments.as_slice() else {
            return None;
        };
        let (builtin, parameters, result) = match (segment.name.as_str(), method_name.as_str()) {
            ("Path", "from_text") => {
                let text = self.types().builtin(BuiltinType::Text);
                let path = self.types().builtin(BuiltinType::Path);
                let definition = self.analyzer.canonical_std_items.path_error;
                let error = self.canonical_std_type(definition, "std.path.PathError", expression);
                (
                    BuiltinValue::PathFromText,
                    vec![text],
                    self.types().intern(TyData::Result { ok: path, error }),
                )
            }
            _ => return None,
        };
        if !type_arguments.is_empty() {
            self.error_at(
                "TypeMismatch",
                "standard static methods do not accept explicit type arguments",
                expression,
            );
        }
        self.check_fixed_arguments(expression, arguments, &parameters);
        self.finish_call_arguments(arguments);
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
        Some(result)
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn check_builtin_value_method_call(
        &mut self,
        expression: ExprId,
        receiver: ExprId,
        receiver_type: BuiltinType,
        method_name: &Name,
        type_arguments: &[TypeRefId],
        arguments: &[ExprId],
    ) -> Option<TyId> {
        let text = self.types().builtin(BuiltinType::Text);
        let int = self.types().builtin(BuiltinType::Int);
        let bool_ty = self.types().builtin(BuiltinType::Bool);
        let (builtin, receiver_passing, parameters, result) =
            match (receiver_type, method_name.as_str()) {
                (BuiltinType::Text, "length") => (
                    BuiltinValue::TextLength,
                    ReceiverPassing::Value,
                    Vec::new(),
                    int,
                ),
                (BuiltinType::Text, "get") => (
                    BuiltinValue::TextGet,
                    ReceiverPassing::Value,
                    vec![int],
                    self.types().intern(TyData::Option(text)),
                ),
                (BuiltinType::Text, "concat") => (
                    BuiltinValue::TextConcat,
                    ReceiverPassing::Value,
                    vec![text],
                    text,
                ),
                (BuiltinType::Text, "contains") => (
                    BuiltinValue::TextContains,
                    ReceiverPassing::Value,
                    vec![text],
                    bool_ty,
                ),
                (BuiltinType::Text, "encode_utf8") => (
                    BuiltinValue::TextEncodeUtf8,
                    ReceiverPassing::Value,
                    Vec::new(),
                    self.types().builtin(BuiltinType::Bytes),
                ),
                (BuiltinType::Bytes, "length") => (
                    BuiltinValue::BytesLength,
                    ReceiverPassing::Value,
                    Vec::new(),
                    int,
                ),
                (BuiltinType::Bytes, "get") => (
                    BuiltinValue::BytesGet,
                    ReceiverPassing::Value,
                    vec![int],
                    self.types().intern(TyData::Option(int)),
                ),
                (BuiltinType::Bytes, "add") => (
                    BuiltinValue::BytesAdd,
                    ReceiverPassing::InOut,
                    vec![int],
                    self.types().builtin(BuiltinType::Unit),
                ),
                (BuiltinType::Bytes, "append") => {
                    let bytes = self.types().builtin(BuiltinType::Bytes);
                    (
                        BuiltinValue::BytesAppend,
                        ReceiverPassing::Value,
                        vec![bytes],
                        bytes,
                    )
                }
                (BuiltinType::Bytes, "decode_utf8") => {
                    let definition = self.analyzer.canonical_std_items.decode_text_error;
                    let error =
                        self.canonical_std_type(definition, "std.text.DecodeTextError", expression);
                    (
                        BuiltinValue::BytesDecodeUtf8,
                        ReceiverPassing::Value,
                        Vec::new(),
                        self.types().intern(TyData::Result { ok: text, error }),
                    )
                }
                (BuiltinType::Path, "as_text") => (
                    BuiltinValue::PathAsText,
                    ReceiverPassing::Value,
                    Vec::new(),
                    text,
                ),
                (BuiltinType::Path, "join") => {
                    let path = self.types().builtin(BuiltinType::Path);
                    let definition = self.analyzer.canonical_std_items.path_error;
                    let error =
                        self.canonical_std_type(definition, "std.path.PathError", expression);
                    (
                        BuiltinValue::PathJoin,
                        ReceiverPassing::Value,
                        vec![path],
                        self.types().intern(TyData::Result { ok: path, error }),
                    )
                }
                (BuiltinType::Duration, "as_milliseconds") => (
                    BuiltinValue::DurationAsMilliseconds,
                    ReceiverPassing::Value,
                    Vec::new(),
                    self.types().builtin(BuiltinType::Int),
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
        if receiver_passing == ReceiverPassing::InOut
            && self
                .semantics
                .expression_places
                .get(receiver)
                .is_none_or(|place| place.mutability != Mutability::Mutable)
        {
            self.error_at(
                "MutReceiverRequiresVar",
                "Bytes.add requires a mutable `var` receiver",
                receiver,
            );
        }
        self.with_inout_argument_scope(
            receiver,
            receiver_passing == ReceiverPassing::InOut,
            |checker| checker.check_fixed_arguments(expression, arguments, &parameters),
        );
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
            .flat_map(|map| {
                map.entries(
                    Namespace::Concept,
                    self.analyzer
                        .program
                        .source_map
                        .definition(self.environment.owner)
                        .unwrap_or_default()
                        .file,
                )
            })
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
            let scope = ImplScope::for_module(self.analyzer.program, module);
            let mut solver = crate::ConformanceSolver::new_in_scope(index, types, scope);
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
            let (return_ty, substitution) = self.with_inout_argument_scope(
                receiver_expression,
                signature.receiver == Some(ReceiverKind::Mutable),
                |checker| {
                    checker.check_callable_arguments(
                        expression,
                        &signature,
                        arguments,
                        &explicit,
                        expected,
                        Substitution::default(),
                    )
                },
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
        let (return_ty, substitution) = self.with_inout_argument_scope(
            receiver_expression,
            signature.receiver == Some(ReceiverKind::Mutable),
            |checker| {
                checker.check_callable_arguments(
                    expression,
                    &signature,
                    arguments,
                    &explicit,
                    expected,
                    Substitution::default(),
                )
            },
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
            TyData::TextMap(value) => {
                let value = self.instantiate_concept_type(value, concrete_self, instance);
                self.types().intern(TyData::TextMap(value))
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
            self.check_call_receiver(*receiver, Some(self_ty));
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
            self.reject_explicit_dispose_call(requirement, receiver);
            let declared_task_carrier = self.witness_declares_task_receiver(&witness);
            self.transfer_task_receiver(
                receiver,
                self_ty,
                declared_task_carrier,
                signature.is_async,
            );
        }
        let explicit = self.resolve_call_type_arguments(type_arguments);
        let (return_ty, substitution) = self.with_optional_inout_argument_scope(
            receiver,
            signature.receiver == Some(ReceiverKind::Mutable),
            |checker| {
                checker.check_callable_arguments(
                    expression,
                    &signature,
                    call_arguments,
                    &explicit,
                    expected,
                    Substitution::default(),
                )
            },
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

    fn reject_explicit_dispose_call(&mut self, requirement: DefId, receiver: ExprId) {
        if self.analyzer.canonical_concepts.dispose_requirement != Some(requirement) {
            return;
        }
        self.error_at(
            "ManualDisposeOfScopedValue",
            "Dispose is reserved for automatic scoped cleanup and cannot be invoked directly",
            receiver,
        );
    }

    fn check_variant_constructor(
        &mut self,
        expression: ExprId,
        variant: DefId,
        owner: DefId,
        call_type_arguments: Option<&[TypeRefId]>,
        arguments: &[ExprId],
        expected: Option<TyId>,
    ) -> TyId {
        let generic_params = self.analyzer.type_generic_params(owner);
        let Some(Signature::Variant { payload, .. }) =
            self.analyzer.typed.signatures.get(variant).cloned()
        else {
            return self.types().error();
        };
        let called_value_constructor = payload.is_empty() && call_type_arguments.is_some();
        if called_value_constructor {
            self.error_at(
                "TypeMismatch",
                "value constructor is not callable",
                expression,
            );
        }
        let explicit = self.resolve_call_type_arguments(call_type_arguments.unwrap_or_default());
        if !called_value_constructor && explicit.len() > generic_params.len() {
            self.error_at(
                "TypeMismatch",
                "too many explicit generic arguments",
                expression,
            );
        }
        let mut substitution = Substitution::default();
        for (parameter, ty) in generic_params.iter().zip(explicit) {
            substitution.insert(*parameter, ty);
        }
        if let Some(expected) = expected {
            let pattern = self.analyzer.nominal_self_type(owner);
            unify_type(
                &self.analyzer.typed.types,
                pattern,
                expected,
                &mut substitution,
            );
        }
        if !called_value_constructor && payload.len() != arguments.len() {
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
        let definition = self
            .analyzer
            .canonical_prelude_type(path)
            .or_else(|| resolver.resolve_definition(path, Namespace::Type).ok());
        let Some(definition) = definition else {
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
        let protected = [
            (self.analyzer.canonical_std_items.file, "std.file.File"),
            (self.analyzer.canonical_std_items.io_error, "std.io.IoError"),
            (self.analyzer.canonical_std_items.socket, "std.net.Socket"),
        ]
        .into_iter()
        .find_map(|(candidate, name)| (candidate == Some(definition)).then_some(name));
        if let Some(name) = protected {
            self.error_at(
                "TypeMismatch",
                format!("{name} values cannot be constructed directly"),
                expression,
            );
            return self.types().error();
        }
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
        let proof = record
            .invariant
            .map(|_| self.invariant_proof(definition, &canonical));
        let field_values = canonical
            .iter()
            .map(|(_, value)| *value)
            .collect::<Vec<_>>();
        self.semantics.record_fields.insert(expression, canonical);
        let nominal = self.types().intern(TyData::Nominal {
            definition,
            arguments,
        });
        let carries_task = self.has_task_obligation(nominal, &mut BTreeSet::new(), 0);
        match proof {
            None => nominal,
            Some(ProofResult::Proven) => {
                self.semantics
                    .construction_checks
                    .insert(expression, ConstructionCheck::Proven);
                nominal
            }
            Some(ProofResult::Disproven) => {
                if carries_task {
                    for value in &field_values {
                        self.consume_task_obligation(*value);
                    }
                }
                self.semantics
                    .construction_checks
                    .insert(expression, ConstructionCheck::Runtime);
                let name = self.analyzer.program.definitions[definition]
                    .name
                    .as_ref()
                    .map_or("<record>", Name::as_str);
                self.error_at(
                    "InvariantUnsatisfied",
                    format!("record literal is statically known to violate `{name}` invariant"),
                    expression,
                );
                self.types().error()
            }
            Some(ProofResult::Unknown) => {
                if carries_task {
                    self.error_at(
                        "TaskFallibleConstructionUnsupported",
                        "a Task-carrying invariant record requires a statically proven invariant because a failed runtime check cannot discard its Task",
                        expression,
                    );
                    for value in field_values {
                        self.consume_task_obligation(value);
                    }
                    return self.types().error();
                }
                self.semantics
                    .construction_checks
                    .insert(expression, ConstructionCheck::Runtime);
                let constraint_error = self.types().builtin(BuiltinType::ConstraintError);
                self.types().intern(TyData::Result {
                    ok: nominal,
                    error: constraint_error,
                })
            }
        }
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
        let module = self.analyzer.program.definitions[self.environment.owner].module;
        let scope = ImplScope::for_module(self.analyzer.program, module);
        let index = &self.analyzer.impl_index;
        let types = &mut self.analyzer.typed.types;
        let mut solver = crate::ConformanceSolver::new_in_scope(index, types, scope);
        match solver.solve(&goal, &environment) {
            Ok(witness) => Some(witness),
            Err(failure) => {
                let (code, message) = match failure {
                    SolveFailure::Missing => (
                        "MissingConformance",
                        "no conformance satisfies this concept requirement".to_owned(),
                    ),
                    SolveFailure::Ambiguous(_) => (
                        "DuplicateConformance",
                        "multiple conformances satisfy this concept requirement".to_owned(),
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
        let module = self.analyzer.program.definitions[self.environment.owner].module;
        let scope = ImplScope::for_module(self.analyzer.program, module);
        let mut solver = crate::ConformanceSolver::new_in_scope(
            &self.analyzer.impl_index,
            &mut self.analyzer.typed.types,
            scope,
        );
        solver.solve(&goal, &environment).ok()
    }

    fn has_resource_conformance(
        &mut self,
        ty: TyId,
        concept: Option<DefId>,
    ) -> Option<WitnessSelection> {
        let concept = concept?;
        self.solve_resource_witness(
            ty,
            ConceptInstance {
                concept,
                bindings: Vec::new(),
            },
        )
    }

    fn has_must_scope_obligation_root(&mut self, ty: TyId) -> bool {
        self.has_must_scope_obligation(ty, &mut BTreeSet::new(), 0)
    }

    fn has_must_scope_obligation(
        &mut self,
        ty: TyId,
        active: &mut BTreeSet<TyId>,
        depth: u16,
    ) -> bool {
        if depth >= 128 {
            // Fail closed for a type shape beyond the checker nesting budget.
            return true;
        }
        if matches!(self.types().data(ty), TyData::Task(_)) {
            return false;
        }
        let must_scope = self.analyzer.canonical_concepts.must_scope;
        if self.has_resource_conformance(ty, must_scope).is_some() {
            return true;
        }
        match self.types().data(ty).clone() {
            TyData::Tuple(elements) => elements.into_iter().any(|element| {
                self.has_must_scope_obligation(element, active, depth.saturating_add(1))
            }),
            TyData::List(element)
            | TyData::TextMap(element)
            | TyData::Option(element)
            | TyData::TaskOutcome(element) => {
                self.has_must_scope_obligation(element, active, depth.saturating_add(1))
            }
            TyData::Result { ok, error } => {
                self.has_must_scope_obligation(ok, active, depth.saturating_add(1))
                    || self.has_must_scope_obligation(error, active, depth.saturating_add(1))
            }
            TyData::Nominal {
                definition,
                arguments,
            } => {
                if !active.insert(ty) {
                    return false;
                }
                let parameters = self.analyzer.type_generic_params(definition);
                let substitution = substitution_for(&parameters, &arguments);
                let kind = self.analyzer.program.definitions[definition].kind.clone();
                let contains = match kind {
                    DefinitionKind::RefinedType(refined) => self
                        .analyzer
                        .typed
                        .resolved_type_refs
                        .get(refined.base)
                        .copied()
                        .is_some_and(|base| {
                            let base = self.types().substitute(base, &substitution);
                            self.has_must_scope_obligation(base, active, depth.saturating_add(1))
                        }),
                    DefinitionKind::Record(record) => record.fields.into_iter().any(|field| {
                        let Some(Signature::Field { ty, .. }) =
                            self.analyzer.typed.signatures.get(field).cloned()
                        else {
                            return false;
                        };
                        let field = self.types().substitute(ty, &substitution);
                        self.has_must_scope_obligation(field, active, depth.saturating_add(1))
                    }),
                    DefinitionKind::Enum(enumeration) => {
                        enumeration.variants.into_iter().any(|variant| {
                            let Some(Signature::Variant { payload, .. }) =
                                self.analyzer.typed.signatures.get(variant).cloned()
                            else {
                                return false;
                            };
                            payload.into_iter().any(|payload| {
                                let payload = self.types().substitute(payload, &substitution);
                                self.has_must_scope_obligation(
                                    payload,
                                    active,
                                    depth.saturating_add(1),
                                )
                            })
                        })
                    }
                    _ => false,
                };
                active.remove(&ty);
                contains
            }
            TyData::Error
            | TyData::Never
            | TyData::Builtin(_)
            | TyData::Task(_)
            | TyData::Param(_)
            | TyData::SelfType(_)
            | TyData::Projection { .. }
            | TyData::DynTarget(_)
            | TyData::View { .. } => false,
        }
    }

    fn has_task_obligation(&mut self, ty: TyId, active: &mut BTreeSet<TyId>, depth: u16) -> bool {
        if depth >= 128 {
            // Fail closed for a type shape beyond the checker nesting budget.
            return true;
        }
        if matches!(self.types().data(ty), TyData::Task(_)) {
            return true;
        }
        match self.types().data(ty).clone() {
            TyData::Tuple(elements) => elements
                .into_iter()
                .any(|element| self.has_task_obligation(element, active, depth.saturating_add(1))),
            TyData::List(element)
            | TyData::TextMap(element)
            | TyData::Option(element)
            | TyData::TaskOutcome(element) => {
                self.has_task_obligation(element, active, depth.saturating_add(1))
            }
            TyData::Result { ok, error } => {
                self.has_task_obligation(ok, active, depth.saturating_add(1))
                    || self.has_task_obligation(error, active, depth.saturating_add(1))
            }
            TyData::Nominal {
                definition,
                arguments,
            } => {
                if !active.insert(ty) {
                    return false;
                }
                let parameters = self.analyzer.type_generic_params(definition);
                let substitution = substitution_for(&parameters, &arguments);
                let kind = self.analyzer.program.definitions[definition].kind.clone();
                let contains = match kind {
                    DefinitionKind::RefinedType(refined) => self
                        .analyzer
                        .typed
                        .resolved_type_refs
                        .get(refined.base)
                        .copied()
                        .is_some_and(|base| {
                            let base = self.types().substitute(base, &substitution);
                            self.has_task_obligation(base, active, depth.saturating_add(1))
                        }),
                    DefinitionKind::Record(record) => record.fields.into_iter().any(|field| {
                        let Some(Signature::Field { ty, .. }) =
                            self.analyzer.typed.signatures.get(field).cloned()
                        else {
                            return false;
                        };
                        let field = self.types().substitute(ty, &substitution);
                        self.has_task_obligation(field, active, depth.saturating_add(1))
                    }),
                    DefinitionKind::Enum(enumeration) => {
                        enumeration.variants.into_iter().any(|variant| {
                            let Some(Signature::Variant { payload, .. }) =
                                self.analyzer.typed.signatures.get(variant).cloned()
                            else {
                                return false;
                            };
                            payload.into_iter().any(|payload| {
                                let payload = self.types().substitute(payload, &substitution);
                                self.has_task_obligation(payload, active, depth.saturating_add(1))
                            })
                        })
                    }
                    _ => false,
                };
                active.remove(&ty);
                contains
            }
            TyData::Error
            | TyData::Never
            | TyData::Builtin(_)
            | TyData::Task(_)
            | TyData::Param(_)
            | TyData::SelfType(_)
            | TyData::Projection { .. }
            | TyData::DynTarget(_)
            | TyData::View { .. } => false,
        }
    }

    fn has_unknown_discard_obligation(
        &mut self,
        ty: TyId,
        active: &mut BTreeSet<TyId>,
        depth: u16,
    ) -> bool {
        if depth >= 128 {
            // Without a negative capability bound, an over-budget shape is
            // not statically known to be safe to discard.
            return true;
        }
        match self.types().data(ty).clone() {
            TyData::Param(_) | TyData::SelfType(_) | TyData::Projection { .. } => true,
            TyData::Tuple(elements) => elements.into_iter().any(|element| {
                self.has_unknown_discard_obligation(element, active, depth.saturating_add(1))
            }),
            TyData::List(element)
            | TyData::TextMap(element)
            | TyData::Option(element)
            | TyData::TaskOutcome(element) => {
                self.has_unknown_discard_obligation(element, active, depth.saturating_add(1))
            }
            TyData::Result { ok, error } => {
                self.has_unknown_discard_obligation(ok, active, depth.saturating_add(1))
                    || self.has_unknown_discard_obligation(error, active, depth.saturating_add(1))
            }
            TyData::Nominal {
                definition,
                arguments,
            } => {
                if !active.insert(ty) {
                    return false;
                }
                let parameters = self.analyzer.type_generic_params(definition);
                let substitution = substitution_for(&parameters, &arguments);
                let kind = self.analyzer.program.definitions[definition].kind.clone();
                let contains = match kind {
                    DefinitionKind::RefinedType(refined) => self
                        .analyzer
                        .typed
                        .resolved_type_refs
                        .get(refined.base)
                        .copied()
                        .is_some_and(|base| {
                            let base = self.types().substitute(base, &substitution);
                            self.has_unknown_discard_obligation(
                                base,
                                active,
                                depth.saturating_add(1),
                            )
                        }),
                    DefinitionKind::Record(record) => record.fields.into_iter().any(|field| {
                        let Some(Signature::Field { ty, .. }) =
                            self.analyzer.typed.signatures.get(field).cloned()
                        else {
                            return false;
                        };
                        let field = self.types().substitute(ty, &substitution);
                        self.has_unknown_discard_obligation(field, active, depth.saturating_add(1))
                    }),
                    DefinitionKind::Enum(enumeration) => {
                        enumeration.variants.into_iter().any(|variant| {
                            let Some(Signature::Variant { payload, .. }) =
                                self.analyzer.typed.signatures.get(variant).cloned()
                            else {
                                return false;
                            };
                            payload.into_iter().any(|payload| {
                                let payload = self.types().substitute(payload, &substitution);
                                self.has_unknown_discard_obligation(
                                    payload,
                                    active,
                                    depth.saturating_add(1),
                                )
                            })
                        })
                    }
                    _ => false,
                };
                active.remove(&ty);
                contains
            }
            TyData::Error
            | TyData::Never
            | TyData::Builtin(_)
            | TyData::Task(_)
            | TyData::DynTarget(_)
            | TyData::View { .. } => false,
        }
    }

    fn may_have_task_obligation(&mut self, ty: TyId) -> bool {
        self.has_task_obligation(ty, &mut BTreeSet::new(), 0)
            || self.has_unknown_discard_obligation(ty, &mut BTreeSet::new(), 0)
    }

    fn seed_task_obligations(&mut self) {
        // Contract and constraint predicates borrow their receiver and
        // parameters. They prove facts about the surrounding value; they do
        // not take ownership of an affine Task obligation from it.
        if self.environment.contract != ContractMode::None {
            return;
        }
        let parameters = self
            .environment
            .params
            .values()
            .copied()
            .collect::<Vec<_>>();
        for (parameter, ty) in parameters {
            if self.has_task_obligation(ty, &mut BTreeSet::new(), 0) {
                self.task_obligations.insert(
                    TaskObligationOwner::Param(parameter),
                    TaskObligationState::Live,
                );
            }
        }
        if let Some(self_ty) = self.environment.self_ty
            && self.has_task_obligation(self_ty, &mut BTreeSet::new(), 0)
        {
            self.task_obligations
                .insert(TaskObligationOwner::SelfValue, TaskObligationState::Live);
        }
    }

    fn register_task_local(&mut self, local: LocalId, ty: TyId) {
        if self.environment.contract != ContractMode::None {
            return;
        }
        if self.has_task_obligation(ty, &mut BTreeSet::new(), 0) {
            self.task_obligations
                .insert(TaskObligationOwner::Local(local), TaskObligationState::Live);
        }
    }

    fn consume_task_owner(&mut self, owner: TaskObligationOwner, expression: ExprId) {
        match self.task_obligations.get(&owner).copied() {
            Some(TaskObligationState::Live) => {
                self.task_obligations
                    .insert(owner, TaskObligationState::Consumed);
            }
            Some(TaskObligationState::Consumed) => self.error_at(
                "TaskAlreadyConsumed",
                "this Task obligation was already consumed",
                expression,
            ),
            Some(TaskObligationState::Conditional) => {
                self.error_at(
                    "TaskConditionallyConsumed",
                    "this Task obligation was consumed on only some control-flow paths",
                    expression,
                );
                self.task_obligations
                    .insert(owner, TaskObligationState::Consumed);
            }
            None => {}
        }
    }

    fn require_live_task_owner(&mut self, expression: ExprId) {
        let Some(place) = self.semantics.expression_places.get(expression).cloned() else {
            return;
        };
        if !place.projections.is_empty() {
            return;
        }
        let owner = match place.root {
            PlaceRoot::Local(local) => TaskObligationOwner::Local(local),
            PlaceRoot::Param(parameter) => TaskObligationOwner::Param(parameter),
            PlaceRoot::SelfValue => TaskObligationOwner::SelfValue,
        };
        match self.task_obligations.get(&owner).copied() {
            Some(TaskObligationState::Consumed) => self.error_at(
                "TaskAlreadyConsumed",
                "this Task-carrying value was already consumed",
                expression,
            ),
            Some(TaskObligationState::Conditional) => self.error_at(
                "TaskConditionallyConsumed",
                "this Task-carrying value is live on only some control-flow paths",
                expression,
            ),
            Some(TaskObligationState::Live) | None => {}
        }
    }

    fn consume_task_obligation(&mut self, expression: ExprId) {
        let Some(ty) = self.semantics.expression_types.get(expression).copied() else {
            return;
        };
        self.consume_task_obligation_with_type(expression, ty);
    }

    fn borrow_task_obligation(&mut self, expression: ExprId) {
        let Some(ty) = self.semantics.expression_types.get(expression).copied() else {
            return;
        };
        if !self.has_task_obligation(ty, &mut BTreeSet::new(), 0) {
            return;
        }
        let Some(place) = self.semantics.expression_places.get(expression).cloned() else {
            return;
        };
        let owner = match place.root {
            PlaceRoot::Local(local) => TaskObligationOwner::Local(local),
            PlaceRoot::Param(parameter) => TaskObligationOwner::Param(parameter),
            PlaceRoot::SelfValue => TaskObligationOwner::SelfValue,
        };
        match self.task_obligations.get(&owner).copied() {
            Some(TaskObligationState::Consumed) => self.error_at(
                "TaskAlreadyConsumed",
                "this Task-carrying value was already consumed",
                expression,
            ),
            Some(TaskObligationState::Conditional) => self.error_at(
                "TaskConditionallyConsumed",
                "this Task-carrying value is live on only some control-flow paths",
                expression,
            ),
            Some(TaskObligationState::Live) | None => {}
        }
    }

    fn reject_task_exit_contract_input(&mut self, expression: ExprId, ty: TyId) {
        if !matches!(self.source().kind, BodyKind::Ensures) || !self.may_have_task_obligation(ty) {
            return;
        }
        self.error_at(
            "TaskExitContractInputUnsupported",
            "an exit contract cannot inspect a Task-carrying input after the body may transfer it; use requires or inspect result",
            expression,
        );
    }

    fn consume_task_obligation_with_type(&mut self, expression: ExprId, ty: TyId) {
        if !self.has_task_obligation(ty, &mut BTreeSet::new(), 0) {
            return;
        }
        if let Some(place) = self.semantics.expression_places.get(expression).cloned() {
            if !place.projections.is_empty() {
                self.error_at(
                    "TaskPartialExtractionUnsupported",
                    "a Task-carrying field cannot be transferred separately before partial-place ownership is available",
                    expression,
                );
                return;
            }
            let owner = match place.root {
                PlaceRoot::Local(local) => TaskObligationOwner::Local(local),
                PlaceRoot::Param(parameter) => TaskObligationOwner::Param(parameter),
                PlaceRoot::SelfValue => TaskObligationOwner::SelfValue,
            };
            self.consume_task_owner(owner, expression);
            return;
        }
        let source = self.source().expressions[expression].clone();
        match source {
            Expr::Tuple(elements) | Expr::List(elements) => {
                for element in elements {
                    self.consume_task_obligation(element);
                }
            }
            Expr::RecordLiteral { fields, .. } => {
                for field in fields {
                    self.consume_task_obligation(field.value);
                }
            }
            Expr::Call { arguments, .. }
                if self.semantics.calls.get(expression).is_some_and(|call| {
                    matches!(
                        &call.target,
                        CallTarget::RefinedConstructor(_)
                            | CallTarget::EnumVariant(_)
                            | CallTarget::Builtin(
                                BuiltinValue::Some | BuiltinValue::Ok | BuiltinValue::Err
                            )
                    )
                }) =>
            {
                for argument in arguments {
                    self.consume_task_obligation(argument);
                }
            }
            _ => {}
        }
    }

    fn check_current_scope_task_obligations(&mut self) {
        let locals = self
            .scopes
            .last()
            .into_iter()
            .flat_map(BTreeMap::values)
            .copied()
            .collect::<Vec<_>>();
        for local in locals {
            let owner = TaskObligationOwner::Local(local);
            if matches!(
                self.task_obligations.get(&owner),
                Some(TaskObligationState::Live | TaskObligationState::Conditional)
            ) {
                self.report_unconsumed_task_owner(owner);
            }
            self.task_obligations.remove(&owner);
        }
    }

    fn check_parameter_task_obligations(&mut self) {
        let owners = self.task_obligations.keys().copied().collect::<Vec<_>>();
        for owner in owners {
            if matches!(owner, TaskObligationOwner::Local(_)) {
                continue;
            }
            if matches!(
                self.task_obligations.get(&owner),
                Some(TaskObligationState::Live | TaskObligationState::Conditional)
            ) {
                self.report_unconsumed_task_owner(owner);
            }
        }
    }

    fn report_unconsumed_task_owner(&mut self, owner: TaskObligationOwner) {
        let (message, span) = match owner {
            TaskObligationOwner::Local(local) => (
                "a stored Task obligation must be awaited, joined, or returned before its lexical scope exits",
                self.local_span(local),
            ),
            TaskObligationOwner::Param(parameter) => (
                "a Task parameter must be awaited, joined, or returned before the callable exits",
                self.param_span(parameter),
            ),
            TaskObligationOwner::SelfValue => (
                "a receiver carrying a Task obligation must be awaited, joined, or returned before the method exits",
                self.body_span(),
            ),
        };
        self.error("UnawaitedAsyncCall", message, span);
    }

    fn audit_task_obligations_at_exit(&mut self) {
        let owners = self.task_obligations.keys().copied().collect::<Vec<_>>();
        for owner in owners {
            if matches!(
                self.task_obligations.get(&owner),
                Some(TaskObligationState::Live | TaskObligationState::Conditional)
            ) {
                self.report_unconsumed_task_owner(owner);
                self.task_obligations
                    .insert(owner, TaskObligationState::Consumed);
            }
        }
    }

    fn diagnose_task_obligations_at_possible_exit(&mut self) {
        let owners = self.task_obligations.keys().copied().collect::<Vec<_>>();
        for owner in owners {
            if matches!(
                self.task_obligations.get(&owner),
                Some(TaskObligationState::Live | TaskObligationState::Conditional)
            ) {
                self.report_unconsumed_task_owner(owner);
            }
        }
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
            TyData::TextMap(value) => {
                let value = self.normalize_type_with_evidence(value, evidence);
                self.types().intern(TyData::TextMap(value))
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
        let mut resolver =
            crate::Resolver::new(self.analyzer.program, self.analyzer.def_maps, module);
        for parameter in self.analyzer.generic_ids_for(self.environment.owner) {
            resolver.add_generic_param(
                self.analyzer.program.generic_params[parameter].name.clone(),
                parameter,
            );
        }
        let owner = if let Some(owner) = self.analyzer.canonical_prelude_type(type_path) {
            owner
        } else {
            match resolver.resolve_type(type_path) {
                Ok(Resolution::Definition(owner)) => owner,
                Ok(_) | Err(_) => return None,
            }
        };
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

    fn pattern_qualifier_matches(&self, path: &Path, expected: TyId) -> bool {
        let Some((_, qualifier)) = path.segments.split_last() else {
            return false;
        };
        if qualifier.is_empty() {
            return true;
        }
        let expected_name = match self.analyzer.typed.types.data(expected) {
            TyData::Option(_) => Some("Option"),
            TyData::Result { .. } => Some("Result"),
            TyData::TaskOutcome(_) => Some("TaskOutcome"),
            TyData::Nominal { definition, .. } => {
                let module = self.analyzer.program.definitions[self.environment.owner].module;
                let mut type_resolver =
                    crate::Resolver::new(self.analyzer.program, self.analyzer.def_maps, module);
                for parameter in self.analyzer.generic_ids_for(self.environment.owner) {
                    type_resolver.add_generic_param(
                        self.analyzer.program.generic_params[parameter].name.clone(),
                        parameter,
                    );
                }
                let type_path = Path {
                    segments: qualifier.to_vec(),
                };
                let qualified_owner = self.analyzer.canonical_prelude_type(&type_path).or_else(
                    || match type_resolver.resolve_type(&type_path) {
                        Ok(Resolution::Definition(owner)) => Some(owner),
                        Ok(_) | Err(_) => None,
                    },
                );
                return qualified_owner == Some(*definition);
            }
            _ => None,
        };
        qualifier.len() == 1
            && expected_name
                .is_some_and(|expected_name| qualifier[0].name.as_str() == expected_name)
    }

    #[allow(clippy::too_many_lines)]
    fn resolve_pattern_variant(
        &self,
        path: &Path,
        payload: &[PatternId],
        expected: TyId,
    ) -> Option<PatternVariant> {
        if !self.pattern_qualifier_matches(path, expected) {
            return None;
        }
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
        rows: &[Vec<CheckedPattern>],
        expression: ExprId,
    ) {
        if self.pattern_vector_useful(&[scrutinee], rows, &[CheckedPattern::Wildcard], 0) {
            self.error_at(
                "NonExhaustiveMatch",
                "match does not cover every possible value",
                expression,
            );
        }
    }

    fn pattern_is_useful(
        &mut self,
        scrutinee: TyId,
        rows: &[Vec<CheckedPattern>],
        candidate: &CheckedPattern,
    ) -> bool {
        self.pattern_vector_useful(&[scrutinee], rows, std::slice::from_ref(candidate), 0)
    }

    fn pattern_vector_useful(
        &mut self,
        expected: &[TyId],
        rows: &[Vec<CheckedPattern>],
        candidate: &[CheckedPattern],
        depth: u16,
    ) -> bool {
        if expected.is_empty() || candidate.is_empty() {
            return rows.is_empty();
        }
        if depth >= 128 {
            // Analysis limits must never make a valid arm look unreachable.
            return true;
        }
        let expected_head = expected[0];
        let expected_tail = &expected[1..];
        let candidate_tail = &candidate[1..];
        match &candidate[0] {
            CheckedPattern::Invalid => true,
            CheckedPattern::Wildcard => {
                if let Some(constructors) = self.finite_pattern_constructors(expected_head) {
                    constructors.into_iter().any(|(head, payload_types)| {
                        let specialized_rows =
                            specialize_checked_rows(rows, &head, payload_types.len());
                        let mut specialized_types = payload_types;
                        specialized_types.extend_from_slice(expected_tail);
                        let mut specialized_candidate = vec![
                            CheckedPattern::Wildcard;
                            specialized_types.len()
                                - expected_tail.len()
                        ];
                        specialized_candidate.extend_from_slice(candidate_tail);
                        self.pattern_vector_useful(
                            &specialized_types,
                            &specialized_rows,
                            &specialized_candidate,
                            depth + 1,
                        )
                    })
                } else {
                    let default_rows = default_checked_rows(rows);
                    self.pattern_vector_useful(
                        expected_tail,
                        &default_rows,
                        candidate_tail,
                        depth + 1,
                    )
                }
            }
            CheckedPattern::Literal(literal) => {
                let head = CheckedPatternHead::Literal(literal.clone());
                let specialized_rows = specialize_checked_rows(rows, &head, 0);
                self.pattern_vector_useful(
                    expected_tail,
                    &specialized_rows,
                    candidate_tail,
                    depth + 1,
                )
            }
            CheckedPattern::Variant(variant, payload) => {
                let payload_types = self.variant_payload(*variant, expected_head);
                if payload_types.len() != payload.len() {
                    return true;
                }
                let head = CheckedPatternHead::Variant(*variant);
                let specialized_rows = specialize_checked_rows(rows, &head, payload_types.len());
                let mut specialized_types = payload_types;
                specialized_types.extend_from_slice(expected_tail);
                let mut specialized_candidate = payload.clone();
                specialized_candidate.extend_from_slice(candidate_tail);
                self.pattern_vector_useful(
                    &specialized_types,
                    &specialized_rows,
                    &specialized_candidate,
                    depth + 1,
                )
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn finite_pattern_constructors(
        &mut self,
        expected: TyId,
    ) -> Option<Vec<(CheckedPatternHead, Vec<TyId>)>> {
        let constructors = match self.analyzer.typed.types.data(expected).clone() {
            TyData::Error | TyData::Never => Vec::new(),
            TyData::Builtin(BuiltinType::Bool) => vec![
                (
                    CheckedPatternHead::Literal(Literal::Bool(false)),
                    Vec::new(),
                ),
                (CheckedPatternHead::Literal(Literal::Bool(true)), Vec::new()),
            ],
            TyData::Builtin(BuiltinType::Unit) => {
                vec![(CheckedPatternHead::Literal(Literal::Unit), Vec::new())]
            }
            TyData::Option(_) => [PatternVariant::None, PatternVariant::Some]
                .into_iter()
                .map(|variant| {
                    (
                        CheckedPatternHead::Variant(variant),
                        self.variant_payload(variant, expected),
                    )
                })
                .collect(),
            TyData::Result { .. } => [PatternVariant::Ok, PatternVariant::Err]
                .into_iter()
                .map(|variant| {
                    (
                        CheckedPatternHead::Variant(variant),
                        self.variant_payload(variant, expected),
                    )
                })
                .collect(),
            TyData::TaskOutcome(_) => [
                PatternVariant::TaskCompleted,
                PatternVariant::TaskFaulted,
                PatternVariant::TaskCancelled,
            ]
            .into_iter()
            .map(|variant| {
                (
                    CheckedPatternHead::Variant(variant),
                    self.variant_payload(variant, expected),
                )
            })
            .collect(),
            TyData::Nominal { definition, .. } => {
                let DefinitionKind::Enum(enumeration) =
                    self.analyzer.program.definitions[definition].kind.clone()
                else {
                    return None;
                };
                enumeration
                    .variants
                    .into_iter()
                    .map(|definition| {
                        let variant = PatternVariant::User(definition);
                        (
                            CheckedPatternHead::Variant(variant),
                            self.variant_payload(variant, expected),
                        )
                    })
                    .collect()
            }
            TyData::Builtin(_)
            | TyData::Tuple(_)
            | TyData::List(_)
            | TyData::TextMap(_)
            | TyData::Task(_)
            | TyData::Param(_)
            | TyData::SelfType(_)
            | TyData::Projection { .. }
            | TyData::DynTarget(_)
            | TyData::View { .. } => return None,
        };
        Some(constructors)
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
            Expr::Call { callee, .. } => {
                let Expr::Path(path) = &self.source().expressions[*callee] else {
                    return self.error_at(
                        "InvalidContractExpression",
                        "expression is outside the pure, effect-restricted contract predicate subset",
                        expression,
                    );
                };
                let module = self.analyzer.program.definitions[self.environment.owner].module;
                let resolved =
                    crate::Resolver::new(self.analyzer.program, self.analyzer.def_maps, module)
                        .resolve_definition(path, Namespace::Value)
                        .ok();
                resolved.is_some_and(|definition| {
                    Some(definition) == self.analyzer.canonical_std_items.is_finite
                })
            }
            Expr::Tuple(_)
            | Expr::List(_)
            | Expr::Block { .. }
            | Expr::If { .. }
            | Expr::MethodCall { .. }
            | Expr::QualifiedMethodCall { .. }
            | Expr::Assign { .. }
            | Expr::RecordLiteral { .. }
            | Expr::Await(_)
            | Expr::Propagate(_)
            | Expr::Return(_) => false,
        };
        if !valid {
            self.error_at(
                "InvalidContractExpression",
                "expression is outside the pure, effect-restricted contract predicate subset",
                expression,
            );
        }
    }

    fn compiler_std_primitive_call(
        &self,
        path: &Path,
    ) -> Option<crate::std_primitives::CompilerStdPrimitive> {
        if self.value_path_lookup(path) != ValuePathLookup::Missing {
            return None;
        }
        let module = self.analyzer.program.definitions[self.environment.owner].module;
        let primitive =
            crate::std_primitives::resolve_local_call(self.analyzer.program, module, path)?;
        self.compiler_std_primitive_owner_is_authorized(primitive)
            .then_some(primitive)
    }

    fn compiler_std_primitive_owner_is_authorized(
        &self,
        primitive: crate::std_primitives::CompilerStdPrimitive,
    ) -> bool {
        use crate::std_primitives::CompilerStdPrimitive as Primitive;

        match primitive {
            Primitive::FileOpenRead => {
                self.is_unique_compiler_std_function(FILE_MODULE, "open_read")
            }
            Primitive::FileCreate => self.is_unique_compiler_std_function(FILE_MODULE, "create"),
            Primitive::FileTryOpenRead => {
                self.is_unique_compiler_std_function(FILE_MODULE, "try_open_read")
            }
            Primitive::FileTryCreate => {
                self.is_unique_compiler_std_function(FILE_MODULE, "try_create")
            }
            Primitive::FileReadText => self
                .is_canonical_inherent_method(self.analyzer.canonical_std_items.file, "read_text"),
            Primitive::FileWriteText => self
                .is_canonical_inherent_method(self.analyzer.canonical_std_items.file, "write_text"),
            Primitive::FileTryReadText => self.is_canonical_inherent_method(
                self.analyzer.canonical_std_items.file,
                "try_read_text",
            ),
            Primitive::FileTryWriteText => self.is_canonical_inherent_method(
                self.analyzer.canonical_std_items.file,
                "try_write_text",
            ),
            Primitive::FileClose => {
                self.is_canonical_dispose_method(self.analyzer.canonical_std_items.file)
            }
            Primitive::SocketConnect => self.is_unique_compiler_std_function(NET_MODULE, "connect"),
            Primitive::SocketTryConnect => {
                self.is_unique_compiler_std_function(NET_MODULE, "try_connect")
            }
            Primitive::SocketReadText => self.is_canonical_inherent_method(
                self.analyzer.canonical_std_items.socket,
                "read_text",
            ),
            Primitive::SocketWriteText => self.is_canonical_inherent_method(
                self.analyzer.canonical_std_items.socket,
                "write_text",
            ),
            Primitive::SocketTryReadText => self.is_canonical_inherent_method(
                self.analyzer.canonical_std_items.socket,
                "try_read_text",
            ),
            Primitive::SocketTryWriteText => self.is_canonical_inherent_method(
                self.analyzer.canonical_std_items.socket,
                "try_write_text",
            ),
            Primitive::SocketClose => {
                self.is_canonical_dispose_method(self.analyzer.canonical_std_items.socket)
            }
            Primitive::IoErrorKind => self
                .is_canonical_inherent_method(self.analyzer.canonical_std_items.io_error, "kind"),
            Primitive::IoErrorMessage => self.is_canonical_inherent_method(
                self.analyzer.canonical_std_items.io_error,
                "message",
            ),
            _ => true,
        }
    }

    fn is_unique_compiler_std_function(&self, module_name: &str, name: &str) -> bool {
        let mut candidates =
            self.analyzer
                .program
                .definitions
                .iter()
                .filter_map(|(definition, item)| {
                    let module = &self.analyzer.program.modules[item.module];
                    if module.package != PackageId::compiler_std(LOOM_LANGUAGE_VERSION)
                        || module.name.as_str() != module_name
                        || item
                            .name
                            .as_ref()
                            .is_none_or(|candidate| candidate.as_str() != name)
                        || !matches!(item.kind, DefinitionKind::Function(_))
                    {
                        return None;
                    }
                    Some(definition)
                });
        matches!(
            (candidates.next(), candidates.next()),
            (Some(definition), None) if definition == self.environment.owner
        )
    }

    fn is_canonical_inherent_method(&self, target: Option<DefId>, name: &str) -> bool {
        let Some(target) = target else {
            return false;
        };
        let target_module = self.analyzer.program.definitions[target].module;
        let mut candidates =
            self.analyzer
                .program
                .definitions
                .iter()
                .filter_map(|(definition, item)| {
                    if item.module != target_module
                        || item
                            .name
                            .as_ref()
                            .is_none_or(|candidate| candidate.as_str() != name)
                    {
                        return None;
                    }
                    let DefinitionKind::Method(method) = &item.kind else {
                        return None;
                    };
                    let DefinitionKind::InherentImpl(implementation) =
                        &self.analyzer.program.definitions[method.owner].kind
                    else {
                        return None;
                    };
                    matches!(
                        self.analyzer
                            .typed
                            .resolved_type_refs
                            .get(implementation.target)
                            .map(|ty| self.analyzer.typed.types.data(*ty)),
                        Some(TyData::Nominal {
                            definition,
                            arguments,
                        }) if *definition == target && arguments.is_empty()
                    )
                    .then_some(definition)
                });
        matches!(
            (candidates.next(), candidates.next()),
            (Some(definition), None) if definition == self.environment.owner
        )
    }

    fn is_canonical_dispose_method(&self, target: Option<DefId>) -> bool {
        let (Some(target), Some(dispose), Some(requirement)) = (
            target,
            self.analyzer.canonical_concepts.dispose,
            self.analyzer.canonical_concepts.dispose_requirement,
        ) else {
            return false;
        };
        let definition = &self.analyzer.program.definitions[self.environment.owner];
        if definition.module != self.analyzer.program.definitions[target].module {
            return false;
        }
        let DefinitionKind::Method(method) = &definition.kind else {
            return false;
        };
        let DefinitionKind::Conformance(_) = &self.analyzer.program.definitions[method.owner].kind
        else {
            return false;
        };
        let Some(conformance) = self.analyzer.typed.conformances.get(method.owner) else {
            return false;
        };
        conformance.concept.concept == dispose
            && matches!(
                self.analyzer.typed.types.data(conformance.target),
                TyData::Nominal {
                    definition,
                    arguments,
                } if *definition == target && arguments.is_empty()
            )
            && conformance.methods.get(&requirement) == Some(&self.environment.owner)
    }

    fn flow_state(&self) -> FlowState {
        FlowState {
            self_dirty: self.self_dirty,
            borrows: self.borrows.clone(),
            pending_must_scope_locals: self.pending_must_scope_locals.clone(),
            transferred_must_scope_locals: self.transferred_must_scope_locals.clone(),
            active_no_suspend: self.active_no_suspend.clone(),
            proof_facts: self.proof_facts.clone(),
            local_terms: self.local_terms.clone(),
            task_obligations: self.task_obligations.clone(),
        }
    }

    fn invalidate_all_proofs(&mut self) {
        self.proof_facts = ProofFacts::default();
        self.local_terms.clear();
    }

    fn apply_defer_effects(&mut self, state: &mut FlowState, defer_base: usize) {
        if self.active_defer_effects.len() <= defer_base {
            return;
        }
        state.proof_facts = ProofFacts::default();
        state.local_terms.clear();
        let effects = self.active_defer_effects[defer_base..].to_vec();
        for effect in effects.iter().rev() {
            if state.self_dirty {
                for expression in &effect.self_boundaries {
                    if self.reported_defer_self_boundaries.insert(*expression) {
                        self.error_at(
                            "InvariantIsolationViolation",
                            "a defer cleanup cannot use `self` after an earlier cleanup or scope exit has invalidated its invariant",
                            *expression,
                        );
                    }
                }
            }
            state.self_dirty |= effect.makes_self_dirty;
        }
    }

    fn check_loop_backedge_state(
        &mut self,
        header: &FlowState,
        backedge: &FlowState,
        expression: ExprId,
    ) {
        if header.self_dirty != backedge.self_dirty {
            self.error_at(
                "LoopReceiverInvariantNotRestored",
                "every continuing loop path must restore the receiver invariant state before the next iteration",
                expression,
            );
        }
        if header.borrows != backedge.borrows
            || header.active_no_suspend != backedge.active_no_suspend
        {
            self.error_at(
                "LoopBorrowStateNotRestored",
                "every continuing loop path must restore its borrow and NoSuspend state before the next iteration",
                expression,
            );
        }
        if header.task_obligations != backedge.task_obligations
            || header.pending_must_scope_locals != backedge.pending_must_scope_locals
            || header.transferred_must_scope_locals != backedge.transferred_must_scope_locals
        {
            self.error_at(
                "LoopObligationStateNotRestored",
                "every continuing loop path must restore its Task and MustScope obligations before the next iteration",
                expression,
            );
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
        self.pending_must_scope_locals
            .clone_from(&state.pending_must_scope_locals);
        self.transferred_must_scope_locals
            .clone_from(&state.transferred_must_scope_locals);
        self.active_no_suspend.clone_from(&state.active_no_suspend);
        self.proof_facts.clone_from(&state.proof_facts);
        self.local_terms.clone_from(&state.local_terms);
        self.task_obligations.clone_from(&state.task_obligations);
    }

    fn join_flow_states(&mut self, states: impl IntoIterator<Item = FlowState>) {
        let states = states.into_iter().collect::<Vec<_>>();
        let mut dirty = false;
        let mut borrows = BTreeMap::new();
        for state in &states {
            dirty |= state.self_dirty;
            for borrow in &state.borrows {
                borrows
                    .entry(borrow.identity)
                    .or_insert_with(|| borrow.clone());
            }
        }
        self.self_dirty = dirty;
        self.borrows = borrows.into_values().collect();
        self.pending_must_scope_locals = states
            .iter()
            .flat_map(|state| state.pending_must_scope_locals.iter().copied())
            .collect();
        self.transferred_must_scope_locals = states
            .iter()
            .flat_map(|state| state.transferred_must_scope_locals.iter().copied())
            .collect();
        let mut no_suspend = BTreeMap::new();
        for state in &states {
            for (local, region, span) in &state.active_no_suspend {
                no_suspend.entry((*local, *region)).or_insert(*span);
            }
        }
        self.active_no_suspend = no_suspend
            .into_iter()
            .map(|((local, region), span)| (local, region, span))
            .collect();
        self.proof_facts =
            ProofFacts::intersection(states.iter().map(|state| state.proof_facts.clone()));
        let mut terms = states
            .first()
            .map_or_else(BTreeMap::new, |state| state.local_terms.clone());
        for state in &states[states.len().min(1)..] {
            terms.retain(|local, term| state.local_terms.get(local) == Some(term));
        }
        self.local_terms = terms;
        let owners = states
            .iter()
            .flat_map(|state| state.task_obligations.keys().copied())
            .collect::<BTreeSet<_>>();
        let mut obligations = BTreeMap::new();
        for owner in owners {
            let values = states
                .iter()
                .map(|state| state.task_obligations.get(&owner).copied())
                .collect::<Vec<_>>();
            let first = values.first().copied().flatten();
            let state = if let Some(first) = first
                && values.iter().all(|value| *value == Some(first))
            {
                first
            } else {
                TaskObligationState::Conditional
            };
            obligations.insert(owner, state);
        }
        self.task_obligations = obligations;
    }

    fn register_borrow(
        &mut self,
        owner: Place,
        mutable: bool,
        region: RegionId,
        identity: BorrowIdentity,
        span: Span,
        expression: ExprId,
    ) -> bool {
        if let Some(conflict) = self
            .borrows
            .iter()
            .find(|borrow| {
                places_overlap(&borrow.owner, &owner)
                    && (matches!(identity, BorrowIdentity::InOut(_)) || mutable || borrow.mutable)
            })
            .cloned()
        {
            self.push_borrow_conflict(&conflict, expression);
            return false;
        }
        self.borrows.push(ActiveBorrow {
            owner,
            mutable,
            region,
            identity,
            span,
        });
        true
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
        self.push_borrow_conflict(&conflict, expression);
    }

    fn push_borrow_conflict(&mut self, conflict: &ActiveBorrow, expression: ExprId) {
        let (code, message, label) = match conflict.identity {
            BorrowIdentity::InOut(_) => (
                "InoutAliasConflict",
                "argument expression aliases an active mutable receiver",
                "exclusive mutable receiver access begins here",
            ),
            BorrowIdentity::Interface(_) if conflict.mutable => (
                "BorrowConflict",
                "argument use conflicts with an active interface call",
                "active interface access begins here",
            ),
            BorrowIdentity::Interface(_) => (
                "ReadonlyBorrowConflict",
                "argument use conflicts with an active interface call",
                "active interface access begins here",
            ),
        };
        self.analyzer.diagnostics.push(
            Diagnostic::error(code, message, self.expr_span(expression))
                .with_label(conflict.span, label),
        );
    }

    fn with_inout_argument_scope<Result>(
        &mut self,
        receiver: ExprId,
        enabled: bool,
        check: impl FnOnce(&mut Self) -> Result,
    ) -> Result {
        let receiver_place = self.semantics.expression_places.get(receiver).cloned();
        let receiver_uses_self = receiver_place
            .as_ref()
            .is_some_and(|place| matches!(place.root, PlaceRoot::SelfValue));
        let dirty_before_arguments = self.self_dirty;
        let dirties_self = enabled
            && receiver_place.as_ref().is_some_and(|place| {
                self.check_invariant_mutation_boundary(
                    receiver,
                    place,
                    InvariantMutation::ReceiverCall,
                )
            });
        let scope = enabled
            .then_some(receiver_place)
            .flatten()
            .and_then(|owner| {
                let region = *self.regions.last().expect("body has a lexical region");
                let scope = self.next_inout_scope;
                self.next_inout_scope = self.next_inout_scope.saturating_add(1);
                self.register_borrow(
                    owner,
                    false,
                    region,
                    BorrowIdentity::InOut(scope),
                    self.expr_span(receiver),
                    receiver,
                )
                .then_some(scope)
            });
        let result = check(self);
        if let Some(scope) = scope {
            self.borrows
                .retain(|borrow| borrow.identity != BorrowIdentity::InOut(scope));
        }
        if receiver_uses_self && !dirty_before_arguments && self.self_dirty {
            self.error_at(
                "InvariantIsolationViolation",
                "a call argument mutated `self` before this receiver call could begin",
                receiver,
            );
        }
        if dirties_self {
            self.self_dirty = true;
        }
        result
    }

    fn check_receiver_isolation(&mut self, receiver: ExprId) {
        let receiver_is_self = self
            .semantics
            .expression_places
            .get(receiver)
            .is_some_and(|place| matches!(place.root, PlaceRoot::SelfValue));
        if self.cleanup_depth > 0 && receiver_is_self {
            self.current_defer_self_boundaries.insert(receiver);
        }
        if self.self_dirty && receiver_is_self {
            self.error_at(
                "InvariantIsolationViolation",
                "a mutated receiver cannot be used for a nested method call",
                receiver,
            );
        }
    }

    fn check_call_receiver(&mut self, receiver: ExprId, expected: Option<TyId>) -> TyId {
        let previous_projection = self.allow_dirty_self_projection;
        let previous_scoped = self.checking_scoped_receiver;
        self.allow_dirty_self_projection = true;
        self.checking_scoped_receiver = true;
        let ty = self.check_expr(receiver, expected, ExpressionContext::Value);
        self.allow_dirty_self_projection = previous_projection;
        self.checking_scoped_receiver = previous_scoped;
        self.check_receiver_isolation(receiver);
        ty
    }

    /// Rejects mutation through a record whose invariant is outside the
    /// current method's recheck boundary. The one permitted protected prefix
    /// is the current `self` root: its owning method rechecks that invariant on
    /// normal exit.
    fn check_invariant_mutation_boundary(
        &mut self,
        expression: ExprId,
        place: &Place,
        mutation: InvariantMutation,
    ) -> bool {
        if place.projections.is_empty() {
            return false;
        }
        let self_definition =
            match &place.root {
                PlaceRoot::SelfValue => self.environment.self_ty.and_then(|ty| {
                    match self.analyzer.typed.types.data(ty) {
                        TyData::Nominal { definition, .. } => Some(*definition),
                        _ => None,
                    }
                }),
                PlaceRoot::Param(_) | PlaceRoot::Local(_) => None,
            };
        let mut crosses_self_invariant = false;
        for (index, projection) in place.projections.iter().enumerate() {
            let PlaceProjection::Field(field) = projection;
            let DefinitionKind::Field(field) = &self.analyzer.program.definitions[*field].kind
            else {
                continue;
            };
            let owner = field.owner;
            let DefinitionKind::Record(record) = &self.analyzer.program.definitions[owner].kind
            else {
                continue;
            };
            if record.invariant.is_none() {
                continue;
            }
            if index == 0 && self_definition == Some(owner) {
                crosses_self_invariant = true;
                continue;
            }
            self.error_at(
                "InvariantInteriorMutation",
                match mutation {
                    InvariantMutation::Assignment => "assignment cannot cross an invariant-bearing record boundary; replace the complete record or call its `mut self` method",
                    InvariantMutation::ReceiverCall => "a mutable receiver call cannot cross an invariant-bearing record boundary; call a `mut self` method on that record so its invariant is rechecked",
                },
                expression,
            );
            return false;
        }
        crosses_self_invariant && place.mutability == Mutability::Mutable
    }

    fn with_optional_inout_argument_scope<Result>(
        &mut self,
        receiver: Option<ExprId>,
        enabled: bool,
        check: impl FnOnce(&mut Self) -> Result,
    ) -> Result {
        if let Some(receiver) = receiver {
            self.with_inout_argument_scope(receiver, enabled, check)
        } else {
            check(self)
        }
    }

    fn finish_call_arguments(&mut self, arguments: &[ExprId]) {
        for argument in arguments {
            if let Some(view) = self.semantics.views.get(*argument) {
                let token = view.token;
                self.borrows
                    .retain(|borrow| borrow.identity != BorrowIdentity::Interface(token));
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum CheckedPattern {
    Invalid,
    Wildcard,
    Literal(Literal),
    Variant(PatternVariant, Vec<Self>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum CheckedPatternHead {
    Literal(Literal),
    Variant(PatternVariant),
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum PatternVariant {
    None,
    Some,
    Ok,
    Err,
    TaskCompleted,
    TaskFaulted,
    TaskCancelled,
    User(DefId),
}

fn default_checked_rows(rows: &[Vec<CheckedPattern>]) -> Vec<Vec<CheckedPattern>> {
    rows.iter()
        .filter(|row| matches!(row.first(), Some(CheckedPattern::Wildcard)))
        .map(|row| row.iter().skip(1).cloned().collect())
        .collect()
}

fn specialize_checked_rows(
    rows: &[Vec<CheckedPattern>],
    expected: &CheckedPatternHead,
    payload_arity: usize,
) -> Vec<Vec<CheckedPattern>> {
    rows.iter()
        .filter_map(|row| match row.first() {
            Some(CheckedPattern::Wildcard) => {
                let mut specialized = vec![CheckedPattern::Wildcard; payload_arity];
                specialized.extend(row.iter().skip(1).cloned());
                Some(specialized)
            }
            Some(CheckedPattern::Literal(actual))
                if expected == &CheckedPatternHead::Literal(actual.clone()) =>
            {
                Some(row.iter().skip(1).cloned().collect())
            }
            Some(CheckedPattern::Variant(actual, payload))
                if expected == &CheckedPatternHead::Variant(*actual)
                    && payload.len() == payload_arity =>
            {
                let mut specialized = payload.clone();
                specialized.extend(row.iter().skip(1).cloned());
                Some(specialized)
            }
            _ => None,
        })
        .collect()
}

fn pattern_variant_resolution(variant: PatternVariant) -> Resolution {
    match variant {
        PatternVariant::None => Resolution::Builtin(BuiltinValue::None),
        PatternVariant::Some => Resolution::Builtin(BuiltinValue::Some),
        PatternVariant::Ok => Resolution::Builtin(BuiltinValue::Ok),
        PatternVariant::Err => Resolution::Builtin(BuiltinValue::Err),
        PatternVariant::TaskCompleted => Resolution::Builtin(BuiltinValue::TaskCompleted),
        PatternVariant::TaskFaulted => Resolution::Builtin(BuiltinValue::TaskFaulted),
        PatternVariant::TaskCancelled => Resolution::Builtin(BuiltinValue::TaskCancelled),
        PatternVariant::User(definition) => Resolution::Definition(definition),
    }
}

const fn proof_unary(operator: UnaryOp) -> ProofUnary {
    match operator {
        UnaryOp::Negate => ProofUnary::Negate,
        UnaryOp::Not => ProofUnary::Not,
    }
}

const fn proof_binary(operator: BinaryOp) -> ProofBinary {
    match operator {
        BinaryOp::Add => ProofBinary::Add,
        BinaryOp::Subtract => ProofBinary::Subtract,
        BinaryOp::Multiply => ProofBinary::Multiply,
        BinaryOp::Divide => ProofBinary::Divide,
        BinaryOp::Equal => ProofBinary::Equal,
        BinaryOp::NotEqual => ProofBinary::NotEqual,
        BinaryOp::Less | BinaryOp::Greater => ProofBinary::Less,
        BinaryOp::LessEqual | BinaryOp::GreaterEqual => ProofBinary::LessEqual,
        BinaryOp::And => ProofBinary::And,
        BinaryOp::Or => ProofBinary::Or,
    }
}

fn proof_binary_term(operator: BinaryOp, left: ProofTerm, right: ProofTerm) -> ProofTerm {
    match operator {
        BinaryOp::Greater => ProofTerm::binary(ProofBinary::Less, right, left),
        BinaryOp::GreaterEqual => ProofTerm::binary(ProofBinary::LessEqual, right, left),
        _ => ProofTerm::binary(proof_binary(operator), left, right),
    }
}

fn constant_proof_term(value: &ConstantValue) -> ProofTerm {
    match value {
        ConstantValue::Bool(value) => ProofTerm::bool(*value),
        ConstantValue::Int(value) => ProofTerm::int(*value),
        ConstantValue::Float(value) => ProofTerm::float(*value),
        ConstantValue::Text(value) => ProofTerm::text(value.clone()),
    }
}

fn proof_place(place: &Place) -> ProofPlace {
    let root = match place.root {
        PlaceRoot::Param(parameter) => ProofRoot::Param(parameter.raw()),
        PlaceRoot::Local(local) => ProofRoot::Local(local.raw()),
        PlaceRoot::SelfValue => ProofRoot::SelfValue,
    };
    let fields = place
        .projections
        .iter()
        .map(|projection| match projection {
            PlaceProjection::Field(field) => *field,
        })
        .collect();
    ProofPlace { root, fields }
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
        TyData::Task(output)
        | TyData::List(output)
        | TyData::TextMap(output)
        | TyData::TaskOutcome(output) => contains_unbound_param(types, *output, substitution),
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
        (TyData::Option(left), TyData::Option(right))
        | (TyData::TextMap(left), TyData::TextMap(right)) => {
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

type ProjectionBindingKey = (TyId, DefId, DefId);

fn projection_binding_has_cycle(
    key: ProjectionBindingKey,
    bindings: &BTreeMap<ProjectionBindingKey, (DefId, TyId)>,
    types: &crate::TyInterner,
    active: &mut BTreeSet<ProjectionBindingKey>,
    depth: u16,
) -> bool {
    if depth >= 128 || !active.insert(key) {
        return true;
    }
    let cyclic = bindings.get(&key).is_some_and(|(_, ty)| {
        let mut references = BTreeSet::new();
        collect_projection_bindings(types, *ty, &mut references, &mut BTreeSet::new(), 0);
        references.into_iter().any(|reference| {
            bindings.contains_key(&reference)
                && projection_binding_has_cycle(reference, bindings, types, active, depth + 1)
        })
    });
    active.remove(&key);
    cyclic
}

fn collect_projection_bindings(
    types: &crate::TyInterner,
    ty: TyId,
    output: &mut BTreeSet<ProjectionBindingKey>,
    visited: &mut BTreeSet<TyId>,
    depth: u16,
) {
    if depth >= 128 || !visited.insert(ty) {
        return;
    }
    match types.data(ty) {
        TyData::Projection {
            self_ty,
            concept,
            associated_type,
        } => {
            output.insert((*self_ty, *concept, *associated_type));
            collect_projection_bindings(types, *self_ty, output, visited, depth + 1);
        }
        TyData::Tuple(elements) => {
            for element in elements {
                collect_projection_bindings(types, *element, output, visited, depth + 1);
            }
        }
        TyData::List(element)
        | TyData::TextMap(element)
        | TyData::Option(element)
        | TyData::Task(element)
        | TyData::TaskOutcome(element) => {
            collect_projection_bindings(types, *element, output, visited, depth + 1);
        }
        TyData::Result { ok, error } => {
            collect_projection_bindings(types, *ok, output, visited, depth + 1);
            collect_projection_bindings(types, *error, output, visited, depth + 1);
        }
        TyData::Nominal { arguments, .. } => {
            for argument in arguments {
                collect_projection_bindings(types, *argument, output, visited, depth + 1);
            }
        }
        TyData::DynTarget(instance) => {
            for binding in &instance.bindings {
                collect_projection_bindings(types, binding.ty, output, visited, depth + 1);
            }
        }
        TyData::View { target, .. } => {
            collect_projection_bindings(types, *target, output, visited, depth + 1);
        }
        TyData::Error
        | TyData::Never
        | TyData::Builtin(_)
        | TyData::Param(_)
        | TyData::SelfType(_) => {}
    }
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
        | TyData::TextMap(task_output)
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
        TyData::TextMap(value) => {
            *value == candidate || is_strict_structural_subterm(types, candidate, *value)
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

fn collect_by_value_nominal_dependencies(
    types: &crate::TyInterner,
    root: TyId,
    nominal_definitions: &BTreeSet<DefId>,
    output: &mut BTreeSet<DefId>,
) {
    let mut pending = vec![root];
    let mut visited = BTreeSet::new();
    while let Some(ty) = pending.pop() {
        if !visited.insert(ty) {
            continue;
        }
        match types.data(ty) {
            TyData::Tuple(elements) => pending.extend(elements.iter().copied()),
            TyData::Option(element) | TyData::TaskOutcome(element) => pending.push(*element),
            TyData::Result { ok, error } => {
                pending.push(*ok);
                pending.push(*error);
            }
            TyData::Nominal {
                definition,
                arguments,
            } => {
                if nominal_definitions.contains(definition) {
                    output.insert(*definition);
                }
                // Nominal arguments participate even when the declaration does
                // not otherwise expose its parameter. This deliberately keeps
                // generic and non-regular cycles syntactic and deterministic.
                pending.extend(arguments.iter().copied());
            }
            // List, TextMap, Task, and View carry their nested value
            // indirectly; the remaining leaves add no nominal dependency.
            TyData::List(_)
            | TyData::TextMap(_)
            | TyData::Task(_)
            | TyData::View { .. }
            | TyData::Error
            | TyData::Never
            | TyData::Builtin(_)
            | TyData::Param(_)
            | TyData::SelfType(_)
            | TyData::Projection { .. }
            | TyData::DynTarget(_) => {}
        }
    }
}

/// Computes strongly connected components without using the process stack.
/// Both passes iterate ordered maps and adjacency lists, so diagnostics remain
/// deterministic across machines.
fn strongly_connected_nominal_components(graph: &BTreeMap<DefId, Vec<DefId>>) -> Vec<Vec<DefId>> {
    let mut visited = BTreeSet::new();
    let mut finishing_order = Vec::with_capacity(graph.len());
    for start in graph.keys().copied() {
        if !visited.insert(start) {
            continue;
        }
        let mut stack = vec![(start, 0_usize)];
        while let Some((node, next_index)) = stack.last_mut() {
            let dependencies = graph.get(node).map_or(&[][..], Vec::as_slice);
            if let Some(next) = dependencies.get(*next_index).copied() {
                *next_index += 1;
                if visited.insert(next) {
                    stack.push((next, 0));
                }
            } else {
                let (finished, _) = stack.pop().expect("DFS stack is not empty");
                finishing_order.push(finished);
            }
        }
    }

    let mut reverse = graph
        .keys()
        .copied()
        .map(|definition| (definition, Vec::new()))
        .collect::<BTreeMap<_, _>>();
    for (definition, dependencies) in graph {
        for dependency in dependencies {
            if let Some(incoming) = reverse.get_mut(dependency) {
                incoming.push(*definition);
            }
        }
    }
    for incoming in reverse.values_mut() {
        incoming.sort_unstable();
        incoming.dedup();
    }

    let mut assigned = BTreeSet::new();
    let mut components = Vec::new();
    while let Some(start) = finishing_order.pop() {
        if !assigned.insert(start) {
            continue;
        }
        let mut component = Vec::new();
        let mut pending = vec![start];
        while let Some(node) = pending.pop() {
            component.push(node);
            if let Some(incoming) = reverse.get(&node) {
                for predecessor in incoming.iter().rev().copied() {
                    if assigned.insert(predecessor) {
                        pending.push(predecessor);
                    }
                }
            }
        }
        components.push(component);
    }
    components
}

fn builtin_type(name: &str) -> Option<BuiltinType> {
    match name {
        "Bool" => Some(BuiltinType::Bool),
        "Int" => Some(BuiltinType::Int),
        "Float" => Some(BuiltinType::Float),
        "Text" => Some(BuiltinType::Text),
        "Bytes" => Some(BuiltinType::Bytes),
        "Path" => Some(BuiltinType::Path),
        "Unit" => Some(BuiltinType::Unit),
        "ConstraintError" => Some(BuiltinType::ConstraintError),
        "ContractFault" => Some(BuiltinType::ContractFault),
        "TaskFault" => Some(BuiltinType::TaskFault),
        "Duration" => Some(BuiltinType::Duration),
        _ => None,
    }
}

fn decode_constant_text(source: &str) -> Result<String, ()> {
    let inner = source
        .strip_prefix('"')
        .and_then(|source| source.strip_suffix('"'))
        .ok_or(())?;
    let mut result = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\\' {
            result.push(character);
            continue;
        }
        match chars.next().ok_or(())? {
            '"' => result.push('"'),
            '\\' => result.push('\\'),
            '/' => result.push('/'),
            'b' => result.push('\u{0008}'),
            'f' => result.push('\u{000c}'),
            'n' => result.push('\n'),
            'r' => result.push('\r'),
            't' => result.push('\t'),
            '0' => result.push('\0'),
            'u' => result.push(decode_constant_unicode_escape(&mut chars)?),
            _ => return Err(()),
        }
    }
    Ok(result)
}

fn decode_constant_unicode_escape<I>(chars: &mut std::iter::Peekable<I>) -> Result<char, ()>
where
    I: Iterator<Item = char>,
{
    if chars.next_if_eq(&'{').is_some() {
        let mut digits = String::new();
        loop {
            let character = chars.next().ok_or(())?;
            if character == '}' {
                break;
            }
            digits.push(character);
        }
        let scalar = u32::from_str_radix(&digits, 16).map_err(|_| ())?;
        return char::from_u32(scalar).ok_or(());
    }

    let mut first = 0_u16;
    for _ in 0..4 {
        let digit = chars
            .next()
            .and_then(|value| value.to_digit(16))
            .ok_or(())?;
        first = (first << 4) | u16::try_from(digit).map_err(|_| ())?;
    }
    if !(0xd800..=0xdfff).contains(&first) {
        return char::from_u32(u32::from(first)).ok_or(());
    }
    if !(0xd800..=0xdbff).contains(&first)
        || chars.next() != Some('\\')
        || chars.next() != Some('u')
    {
        return Err(());
    }
    let mut second = 0_u16;
    for _ in 0..4 {
        let digit = chars
            .next()
            .and_then(|value| value.to_digit(16))
            .ok_or(())?;
        second = (second << 4) | u16::try_from(digit).map_err(|_| ())?;
    }
    if !(0xdc00..=0xdfff).contains(&second) {
        return Err(());
    }
    let scalar = 0x1_0000 + ((u32::from(first) - 0xd800) << 10) + (u32::from(second) - 0xdc00);
    char::from_u32(scalar).ok_or(())
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
    use std::fmt::Write as _;

    use loom_core::{FileId, LOOM_LANGUAGE_VERSION, ModuleName, PackageId};
    use loom_hir::{
        Expr, PackageSourceUnit, Program, SourceUnit, lower_files, lower_package_files,
    };
    use loom_syntax::parse_with_file;

    use super::{Analysis, CONSTANT_EVALUATION_DEPTH_LIMIT, analyze};
    use crate::{
        CallTarget, ConstantValue, ConstructionCheck, RuntimeCheck, Signature, TyData, ViewSource,
        WitnessSelection, WitnessSource,
    };

    const DYNAMIC_SOURCE_FIXTURE: &str = r"
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

    fn analyze_source_with_std_float(source: &str) -> (Program, Analysis) {
        let std_file = FileId(0);
        let application_file = FileId(1);
        let float = parse_with_file(
            std_file,
            include_str!("../../../library/std/float/float.loom"),
        );
        let application = parse_with_file(application_file, source);
        assert!(float.diagnostics().is_empty(), "{:#?}", float.diagnostics());
        assert!(
            application.diagnostics().is_empty(),
            "{:#?}",
            application.diagnostics()
        );

        let std_package = PackageId::compiler_std(LOOM_LANGUAGE_VERSION);
        let application_package = PackageId::new("application", "0");
        let mut lowered = lower_package_files([
            PackageSourceUnit {
                file: std_file,
                package: std_package.clone(),
                module: ModuleName::new("std.float"),
                syntax: float.ast(),
            },
            PackageSourceUnit {
                file: application_file,
                package: application_package.clone(),
                module: ModuleName::new("application"),
                syntax: application.ast(),
            },
        ]);
        assert!(lowered.diagnostics.is_empty(), "{:#?}", lowered.diagnostics);
        lowered
            .program
            .register_package(std_package.clone(), [], false);
        lowered.program.register_package(
            application_package,
            [(loom_core::Name::new("std"), std_package)],
            true,
        );
        let analysis = analyze(&lowered.program);
        (lowered.program, analysis)
    }

    #[test]
    fn constants_are_evaluated_and_expand_in_contract_proofs() {
        let (_, analysis) = analyze_source(
            r#"
pub const base Int = 40
const answer Int = base + 2
const escaped Text = "\u{41}"
const text_matches Bool = escaped == "A"
const short_and Bool = false && (1 / 0 == 0)
const short_or Bool = true || (1 / 0 == 0)
const ratio Float = -(6.0 / 2.0)
const minimum Float = 0.0
const lowest Int = -9223372036854775808

type Money = Float where self >= minimum

fn value() Int
    ensures result == answer
{
    answer
}

fn proven() {
    assert answer == 42
}

fn money() Money { Money(1.0) }
"#,
        );
        assert!(
            analysis.diagnostics.is_empty(),
            "{:#?}",
            analysis.diagnostics
        );
        let values = analysis
            .typed
            .constants
            .values()
            .cloned()
            .collect::<Vec<_>>();
        assert!(values.contains(&ConstantValue::Int(40)));
        assert!(values.contains(&ConstantValue::Int(42)));
        assert!(values.contains(&ConstantValue::Text("A".into())));
        assert!(values.contains(&ConstantValue::Bool(false)));
        assert!(
            values
                .iter()
                .filter(|value| **value == ConstantValue::Bool(true))
                .count()
                >= 2
        );
        assert!(values.contains(&ConstantValue::Float(-3.0)));
        assert!(values.contains(&ConstantValue::Int(i64::MIN)));
        assert!(analysis.typed.bodies.values().any(|semantics| {
            semantics
                .construction_checks
                .values()
                .any(|check| *check == ConstructionCheck::Proven)
        }));
        assert!(analysis.typed.bodies.values().any(|semantics| {
            semantics
                .assertion_checks
                .values()
                .any(|check| *check == RuntimeCheck::Proven)
        }));
    }

    #[test]
    fn constants_reject_runtime_work_cycles_and_unsupported_types() {
        let (_, analysis) = analyze_source(
            r"
fn runtime_value() Int { 1 }
const called Int = runtime_value()
const first Int = second + 1
const second Int = first + 1
const divided Int = 1 / 0
const overflowed Int = 9223372036854775807 + 1
const allocated List[Int] = [1]
",
        );
        let codes = analysis
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect::<Vec<_>>();
        assert!(codes.contains(&"InvalidConstantExpression"), "{codes:?}");
        assert!(codes.contains(&"ConstantCycle"), "{codes:?}");
        assert!(codes.contains(&"ConstantEvaluationFailed"), "{codes:?}");
        assert!(
            codes
                .iter()
                .filter(|code| **code == "ConstantEvaluationFailed")
                .count()
                >= 2,
            "{codes:?}"
        );
        assert!(codes.contains(&"InvalidConstantType"), "{codes:?}");
    }

    #[test]
    fn constant_evaluation_limit_is_independent_of_declaration_and_cache_order() {
        let source = |leaf_first: bool| {
            let count = CONSTANT_EVALUATION_DEPTH_LIMIT + 2;
            let mut declarations = (0..count).collect::<Vec<_>>();
            if leaf_first {
                declarations.reverse();
            }
            let mut source = String::new();
            for index in declarations {
                if index + 1 == count {
                    writeln!(source, "const value{index} Int = 1").expect("write constant leaf");
                } else {
                    writeln!(source, "const value{index} Int = value{}", index + 1)
                        .expect("write constant dependency");
                }
            }
            source
        };
        let limit_diagnostics = |analysis: &Analysis| {
            analysis
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == "ConstantEvaluationLimit")
                .map(|diagnostic| diagnostic.message.clone())
                .collect::<Vec<_>>()
        };

        let (_, dependency_first) = analyze_source(&source(false));
        let (_, leaf_first) = analyze_source(&source(true));
        let dependency_first_limits = limit_diagnostics(&dependency_first);
        let leaf_first_limits = limit_diagnostics(&leaf_first);
        assert_eq!(
            dependency_first_limits.len(),
            2,
            "{:#?}",
            dependency_first.diagnostics
        );
        assert_eq!(leaf_first_limits, dependency_first_limits);
        for analysis in [&dependency_first, &leaf_first] {
            assert!(analysis.has_errors());
            assert!(
                analysis
                    .diagnostics
                    .iter()
                    .all(|diagnostic| diagnostic.code != "ConstantCycle"),
                "{:#?}",
                analysis.diagnostics
            );
        }
    }

    #[test]
    fn constant_evaluation_limit_uses_the_deepest_dag_path() {
        let chain_length = CONSTANT_EVALUATION_DEPTH_LIMIT * 7 / 8;
        let deep_use_depth = CONSTANT_EVALUATION_DEPTH_LIMIT / 6;
        let mut source = String::new();
        for index in 0..chain_length {
            if index + 1 == chain_length {
                writeln!(source, "const shared{index} Int = 1").expect("write constant leaf");
            } else {
                writeln!(source, "const shared{index} Int = shared{}", index + 1)
                    .expect("write constant dependency");
            }
        }
        let mut deep_use = "shared0".to_owned();
        for _ in 0..deep_use_depth {
            deep_use = format!("0 + ({deep_use})");
        }
        writeln!(source, "const root Int = shared0 + ({deep_use})").expect("write constant root");

        let (_, analysis) = analyze_source(&source);
        let limits = analysis
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "ConstantEvaluationLimit")
            .count();
        assert_eq!(limits, 1, "{:#?}", analysis.diagnostics);
        assert!(
            analysis
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.code != "ConstantCycle"),
            "{:#?}",
            analysis.diagnostics
        );
    }

    fn analyze_resource_module(package: PackageId) -> (Program, Analysis) {
        let parsed = parse_with_file(
            FileId(0),
            r"pub concept Dispose {
    method dispose(mut self)
}

pub concept MustScope {}
pub concept NoSuspend {}
",
        );
        assert!(
            parsed.diagnostics().is_empty(),
            "{:#?}",
            parsed.diagnostics()
        );
        let lowered = lower_package_files([PackageSourceUnit {
            file: FileId(0),
            package,
            module: ModuleName::new("std.resource"),
            syntax: parsed.ast(),
        }]);
        assert!(lowered.diagnostics.is_empty(), "{:#?}", lowered.diagnostics);
        let analysis = analyze(&lowered.program);
        (lowered.program, analysis)
    }

    #[test]
    fn resource_language_items_require_the_exact_current_std_package() {
        let non_current_version = "0.5";
        let (_, canonical) =
            analyze_resource_module(PackageId::compiler_std(LOOM_LANGUAGE_VERSION));
        assert!(
            canonical.diagnostics.is_empty(),
            "{:#?}",
            canonical.diagnostics
        );
        assert!(canonical.canonical_concepts.dispose.is_some());
        assert!(canonical.canonical_concepts.dispose_requirement.is_some());
        assert!(canonical.canonical_concepts.must_scope.is_some());
        assert!(canonical.canonical_concepts.no_suspend.is_some());

        for wrong in [
            PackageId::standalone(),
            PackageId::with_language("std", non_current_version, non_current_version),
            PackageId::with_language("std", non_current_version, LOOM_LANGUAGE_VERSION),
            PackageId::with_language("std", LOOM_LANGUAGE_VERSION, non_current_version),
        ] {
            let (_, ordinary) = analyze_resource_module(wrong);
            assert_eq!(
                ordinary.canonical_concepts,
                super::CanonicalConcepts::default()
            );
        }
    }

    #[test]
    fn resolves_generic_data_and_callable_signatures_definition_first() {
        let parsed = parse_with_file(
            FileId(0),
            "record Boxed[T] {\n    value T\n}\n\nfn wrap[T](value T) Boxed[T] {\n    Boxed { value = value }\n}\n",
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
    fn test_signature_uses_implicit_unit_and_rejects_parameters() {
        let parsed = parse_with_file(FileId(0), "test fn ok() {}\n\ntest fn bad(value Int) {}\n");
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
        let (_, valid) = analyze_source("pub fn minimum() Int { -9223372036854775808 }\n");
        assert!(valid.diagnostics.is_empty(), "{:#?}", valid.diagnostics);

        let (_, invalid) = analyze_source("pub fn too_large() Int { 9223372036854775808 }\n");
        assert!(
            invalid
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "IntegerLiteralOutOfRange")
        );
    }

    #[test]
    fn constraints_contracts_source_reaches_body_checker() {
        let (_, analysis) = analyze_source_with_std_float(include_str!(
            "../../../examples/constraints-contracts/shop.loom"
        ));
        assert!(
            analysis.diagnostics.is_empty(),
            "{:#?}",
            analysis.diagnostics
        );
    }

    #[test]
    fn concepts_polymorphism_source_reaches_body_checker() {
        let source = format!(
            "{}\n{}",
            include_str!("../../../examples/concepts-polymorphism/concepts.loom"),
            include_str!("../../../examples/concepts-polymorphism/concepts_test.loom"),
        );
        let parsed = parse_with_file(FileId(0), &source);
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
pub record Pair {
    value Int
    invariant self.value >= 0
}

impl Pair {
    method observe(self) {
    }

    method mutate(mut self) {
        if true {
            self.value = 1
        } else {
            self.observe()
        }
        self.observe()
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
    fn defer_self_effects_follow_scope_loop_control_and_lifo_order() {
        let (_, analysis) = analyze_source(
            r"
pub record Counter {
    value Int
    invariant self.value >= 0
}

impl Counter {
    method observe(self) {
    }

    method nestedScope(mut self) {
        {
            defer {
                self.value = -1
            }
        }
        self.observe()
    }

    method breakExit(mut self) {
        while true {
            defer {
                self.value = -1
            }
            break
        }
        self.observe()
    }

    method lifo(mut self) {
        defer {
            self.observe()
        }
        defer {
            self.value = -1
        }
    }
}
",
        );
        let isolation = analysis
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "InvariantIsolationViolation")
            .count();
        assert_eq!(isolation, 3, "{:#?}", analysis.diagnostics);
    }

    #[test]
    fn loop_backedges_must_restore_receiver_invariant_state() {
        let (_, analysis) = analyze_source(
            r"
pub record Counter {
    value Int
    invariant self.value >= 0
}

impl Counter {
    method observe(self) {
    }

    method rangeDirty(mut self) {
        for index in 0..2 {
            self.value = index
        }
    }

    method whileDirty(mut self) {
        var first = true
        while {
            self.observe()
            first
        } {
            self.value = 0
            first = false
        }
    }
}
",
        );
        let unrestored = analysis
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "LoopReceiverInvariantNotRestored")
            .count();
        assert_eq!(unrestored, 2, "{:#?}", analysis.diagnostics);
    }

    #[test]
    fn successful_assertion_restores_the_receiver_invariant_boundary() {
        let (_, analysis) = analyze_source(
            r"
pub record Counter {
    value Int
    invariant self.value >= 0
}

impl Counter {
    method observe(self) {
    }

    method repair(mut self, next Int) {
        self.value = next
        assert self.value >= 0
        self.observe()
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
    fn nested_variant_binding_does_not_make_match_exhaustive() {
        let (_, analysis) = analyze_source(
            r"
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
    fn nested_boolean_variant_patterns_require_every_payload_case() {
        let (_, incomplete) = analyze_source(
            r"
fn classify(value Option[Bool]) Int {
    match value {
        Some(true) => 1
        None => 0
    }
}
",
        );
        assert!(
            incomplete
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "NonExhaustiveMatch"),
            "{:#?}",
            incomplete.diagnostics
        );

        let (_, complete) = analyze_source(
            r"
fn classify(value Option[Bool]) Int {
    match value {
        Some(true) => 1
        Some(false) => 2
        None => 0
    }
}
",
        );
        assert!(
            complete.diagnostics.is_empty(),
            "{:#?}",
            complete.diagnostics
        );
    }

    #[test]
    fn match_usefulness_reports_duplicate_and_shadowed_arms() {
        let (_, duplicate) = analyze_source(
            r"
fn classify(value Option[Bool]) Int {
    match value {
        Some(true) => 1
        Some(true) => 2
        Some(false) => 3
        None => 0
    }
}
",
        );
        assert_eq!(
            duplicate
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == "UnreachableMatchArm")
                .count(),
            1,
            "{:#?}",
            duplicate.diagnostics
        );
        assert!(
            duplicate
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.code != "NonExhaustiveMatch"),
            "{:#?}",
            duplicate.diagnostics
        );

        let (_, shadowed) = analyze_source(
            r"
fn classify(value Option[Int]) Int {
    match value {
        _ => 1
        None => 0
    }
}
",
        );
        assert!(
            shadowed
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "UnreachableMatchArm"),
            "{:#?}",
            shadowed.diagnostics
        );
    }

    #[test]
    fn equality_requires_a_statically_derivable_value_equality_capability() {
        let (_, generic) = analyze_source(
            r"
fn same[T](left T, right T) Bool {
    left == right
}
",
        );
        assert!(
            generic
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "InvalidGenericOperation"),
            "{:#?}",
            generic.diagnostics
        );

        let (_, aggregate) = analyze_source(
            r"
record Boxed[T] {
    value T
}

fn same[T](left Boxed[T], right Boxed[T]) Bool {
    left == right
}
",
        );
        assert!(
            aggregate
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "InvalidGenericOperation"),
            "{:#?}",
            aggregate.diagnostics
        );

        let (_, dynamic) = analyze_source(
            r"
dyn concept Display {
    method display(self) Text
}

fn same_view(left dyn Display, right dyn Display) Bool {
    left == right
}
",
        );
        assert!(
            dynamic
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "InvalidGenericOperation"),
            "{:#?}",
            dynamic.diagnostics
        );
    }

    #[test]
    fn structural_values_derive_equality_when_every_component_supports_it() {
        let (_, analysis) = analyze_source(
            r"
record Pair {
    names List[Text]
    count Int
}

enum MaybePair {
    Missing
    Present(Pair)
}

fn same(left MaybePair, right MaybePair) Bool {
    left == right
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
    fn record_side_table_preserves_source_evaluation_and_canonical_layout() {
        let (program, analysis) = analyze_source(
            r"
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
    fn associated_type_bindings_must_satisfy_declared_bounds() {
        let (_, valid) = analyze_source(
            r"
concept Display {
    method display(self) Text
}

record Label {
    value Text
}

impl Display for Label {
    method display(self) Text { self.value }
}

concept Source {
    associated type Item: Display
    method read(self) Self.Item
}

record Labels {
    value Label
}

impl Source for Labels {
    associated type Item = Label
    method read(self) Label { self.value }
}
",
        );
        assert!(valid.diagnostics.is_empty(), "{:#?}", valid.diagnostics);

        let (_, invalid) = analyze_source(
            r"
concept Display {
    method display(self) Text
}

concept Source {
    associated type Item: Display
    method read(self) Self.Item
}

record Number {
    value Int
}

impl Source for Number {
    associated type Item = Int
    method read(self) Int { self.value }
}
",
        );
        assert!(
            invalid
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "AssociatedTypeBoundNotSatisfied"),
            "{:#?}",
            invalid.diagnostics
        );
    }

    #[test]
    fn associated_type_bounds_use_conditional_conformance_proofs() {
        let (_, analysis) = analyze_source(
            r"
concept Display {
    method display(self) Text
}

concept Source {
    associated type Item: Display
    method read(self) Self.Item
}

record Boxed[T] {
    value T
}

impl[T: Display] Source for Boxed[T] {
    associated type Item = T
    method read(self) T { self.value }
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
    fn associated_projection_cycles_are_rejected() {
        let (_, analysis) = analyze_source(
            r"
concept Pair {
    associated type First
    associated type Second
}

record Broken {}

impl Pair for Broken {
    associated type First = Self.Second
    associated type Second = Self.First
}
",
        );
        assert!(
            analysis
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "ConformanceResolutionCycle"),
            "{:#?}",
            analysis.diagnostics
        );
    }

    #[test]
    fn concrete_call_normalizes_associated_projection_from_selected_witness() {
        let (_, analysis) = analyze_source(
            r"
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
        let (_, analysis) = analyze_source_with_std_float(
            r"
import std.float.ParseFloatError
import std.float.parse_float

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

    #[test]
    fn loop_control_and_defer_do_not_create_first_iteration_only_proofs() {
        let (_, analysis) = analyze_source(
            r"
type Positive = Int where self > 0

fn afterBreak(flag Bool) Result[Positive, ConstraintError] {
    var value = 1
    while flag {
        value = 0
        break
        value = 1
    }
    Positive(value)
}

fn registrationIsNotExecution(raw Int) Result[Positive, ConstraintError] {
    var value = raw
    defer {
        value = 1
    }
    Positive(value)
}

fn cleanupReadsLate() {
    var value = 1
    defer {
        discard Positive(value)
    }
    value = 0
}

fn repeatedBody() {
    var value = 0
    for index in 0..2 {
        value = value + 1
        discard Positive(value)
    }
}
",
        );
        assert!(
            analysis.diagnostics.is_empty(),
            "{:#?}",
            analysis.diagnostics
        );
        let checks = analysis
            .typed
            .bodies
            .values()
            .flat_map(|body| body.construction_checks.values().copied())
            .collect::<Vec<_>>();
        assert_eq!(checks.len(), 4, "{checks:?}");
        assert!(
            checks
                .iter()
                .all(|check| *check == ConstructionCheck::Runtime),
            "{checks:?}"
        );
    }

    #[test]
    fn constrained_construction_uses_constants_and_established_path_facts() {
        let (_, analysis) = analyze_source_with_std_float(
            r"
import std.float.is_finite

type Money = Float where is_finite(self) && self >= 0.0

fn literal() Money { Money(10.0) }

fn folded() Money { Money(4.0 + 6.0) }

fn checked(raw Float) Result[Money, ConstraintError] { Money(raw) }

fn required(raw Float) Money
    requires is_finite(raw) && raw >= 0.0
{
    Money(raw)
}

fn asserted(raw Float) Money {
    assert is_finite(raw) && raw > 0.0
    Money(raw)
}

fn branched(raw Float) Result[Money, ConstraintError] {
    if is_finite(raw) && raw >= 0.0 {
        Ok(Money(raw))
    } else {
        checked(raw)
    }
}
",
        );
        assert!(
            analysis.diagnostics.is_empty(),
            "{:#?}",
            analysis.diagnostics
        );
        let checks = analysis
            .typed
            .bodies
            .iter()
            .flat_map(|(_, body)| body.construction_checks.values().copied())
            .collect::<Vec<_>>();
        assert_eq!(
            checks
                .iter()
                .filter(|check| **check == ConstructionCheck::Proven)
                .count(),
            5,
            "{checks:?}"
        );
        assert_eq!(
            checks
                .iter()
                .filter(|check| **check == ConstructionCheck::Runtime)
                .count(),
            1,
            "{checks:?}"
        );
    }

    #[test]
    fn constrained_literal_proof_is_independent_of_package_file_order() {
        let application_file = FileId(0);
        let foundation_file = FileId(1);
        let std_file = FileId(2);
        let application = parse_with_file(
            application_file,
            r"import foundation.money.Money

fn maximum_order() Money {
    Money(1000.0)
}
",
        );
        let foundation = parse_with_file(
            foundation_file,
            r"import std.float.is_finite

pub type Money = Float where is_finite(self) && self >= 0.0
",
        );
        let float = parse_with_file(
            std_file,
            include_str!("../../../library/std/float/float.loom"),
        );
        assert!(
            application.diagnostics().is_empty()
                && foundation.diagnostics().is_empty()
                && float.diagnostics().is_empty(),
            "application={:#?}, foundation={:#?}, float={:#?}",
            application.diagnostics(),
            foundation.diagnostics(),
            float.diagnostics()
        );

        let application_package = PackageId::new("application", "0");
        let foundation_package = PackageId::new("foundation", "0");
        let std_package = PackageId::compiler_std(LOOM_LANGUAGE_VERSION);
        let mut lowered = lower_package_files([
            PackageSourceUnit {
                file: application_file,
                package: application_package.clone(),
                module: ModuleName::new("application.config"),
                syntax: application.ast(),
            },
            PackageSourceUnit {
                file: foundation_file,
                package: foundation_package.clone(),
                module: ModuleName::new("foundation.money"),
                syntax: foundation.ast(),
            },
            PackageSourceUnit {
                file: std_file,
                package: std_package.clone(),
                module: ModuleName::new("std.float"),
                syntax: float.ast(),
            },
        ]);
        lowered.program.register_package(
            foundation_package.clone(),
            [(loom_core::Name::new("std"), std_package.clone())],
            false,
        );
        lowered.program.register_package(std_package, [], false);
        lowered.program.register_package(
            application_package,
            [(loom_core::Name::new("foundation"), foundation_package)],
            true,
        );

        let analysis = analyze(&lowered.program);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:#?}",
            analysis.diagnostics
        );
        assert_eq!(
            analysis
                .typed
                .bodies
                .values()
                .flat_map(|body| body.construction_checks.values())
                .copied()
                .collect::<Vec<_>>(),
            [ConstructionCheck::Proven]
        );
    }

    #[test]
    fn invariant_literals_share_the_same_proof_and_runtime_boundary() {
        let (_, analysis) = analyze_source(
            r"
type Money = Float where self >= 0.0

record Range {
    low Money
    high Money
    invariant self.low <= self.high
}

fn literal() Range {
    let low = Money(1.0)
    let high = Money(2.0)
    Range { low = low, high = high }
}

fn checked(low Money, high Money) Result[Range, ConstraintError] {
    Range { low = low, high = high }
}

fn required(low Money, high Money) Range
    requires low <= high
{
    Range { low = low, high = high }
}
",
        );
        assert!(
            analysis.diagnostics.is_empty(),
            "{:#?}",
            analysis.diagnostics
        );
        let checks = analysis
            .typed
            .bodies
            .iter()
            .flat_map(|(_, body)| body.construction_checks.values().copied())
            .collect::<Vec<_>>();
        assert_eq!(
            checks
                .iter()
                .filter(|check| **check == ConstructionCheck::Proven)
                .count(),
            4,
            "{checks:?}"
        );
        assert_eq!(
            checks
                .iter()
                .filter(|check| **check == ConstructionCheck::Runtime)
                .count(),
            1,
            "{checks:?}"
        );
    }

    #[test]
    fn statically_false_constraint_and_invariant_are_source_errors() {
        let (_, analysis) = analyze_source(
            r"
type Money = Float where self >= 0.0

record Range {
    low Money
    high Money
    invariant self.low <= self.high
}

fn bad_money() Money { Money(-1.0) }

fn bad_range() Range {
    let low = Money(2.0)
    let high = Money(1.0)
    Range { low = low, high = high }
}
",
        );
        let codes = analysis
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect::<Vec<_>>();
        assert!(codes.contains(&"ConstraintUnsatisfied"), "{codes:?}");
        assert!(codes.contains(&"InvariantUnsatisfied"), "{codes:?}");
    }

    #[test]
    fn constrained_conversion_matrix_is_one_way_and_check_free_when_established() {
        let (_, valid) = analyze_source(
            r"
type Money = Float where self >= 0.0
type Positive = Float where self > 0.0

fn widen(value Money) Float { value }

fn calculate(value Money) Float { value + 1.0 }

fn reestablish(value Money) Money { Money(value) }

fn positive() Positive { Positive(1.0) }

fn strict_to_loose() Money { Money(positive()) }

fn pair(value Money) (Money, Money) { (value, value) }

fn tuple_binding(value Money) Money {
    let first, second = pair(value)
    Money(first)
}

record Wallet {
    amount Money
    invariant self.amount >= 0.0
}

fn make_money() Money { Money(2.0) }

fn wallet_from_established_field() Wallet {
    Wallet { amount = make_money() }
}
",
        );
        assert!(valid.diagnostics.is_empty(), "{:#?}", valid.diagnostics);
        let valid_checks = valid
            .typed
            .bodies
            .iter()
            .flat_map(|(_, body)| body.construction_checks.values().copied())
            .collect::<Vec<_>>();
        assert_eq!(valid_checks.len(), 6, "{valid_checks:?}");
        assert!(
            valid_checks
                .iter()
                .all(|check| *check == ConstructionCheck::Proven),
            "{valid_checks:?}"
        );

        let (_, invalid) = analyze_source(
            r"
type Money = Float where self >= 0.0
type Percentage = Float where self >= 0.0

fn implicit_narrow(value Float) Money { value }
fn cross_nominal(value Money) Percentage { value }
",
        );
        let mismatches = invalid
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "TypeMismatch")
            .count();
        assert_eq!(mismatches, 2, "{:#?}", invalid.diagnostics);
    }

    #[test]
    fn conformance_implementation_uses_its_requirement_contract_facts() {
        let (_, analysis) = analyze_source(
            r"
type Money = Float where self >= 0.0

concept Factory {
    method make(self, raw Float) Money
        requires raw >= 0.0
}

record DefaultFactory {}

impl Factory for DefaultFactory {
    method make(self, raw Float) Money {
        Money(raw)
    }
}
",
        );
        assert!(
            analysis.diagnostics.is_empty(),
            "{:#?}",
            analysis.diagnostics
        );
        let checks = analysis
            .typed
            .bodies
            .iter()
            .flat_map(|(_, body)| body.construction_checks.values().copied())
            .collect::<Vec<_>>();
        assert_eq!(checks, [ConstructionCheck::Proven]);
    }

    #[test]
    fn loops_and_impure_conditions_do_not_create_stale_proofs() {
        let (_, analysis) = analyze_source(
            r"
type Money = Float where self >= 0.0

record Cell { value Float }

impl Cell {
    method make_negative(mut self) Bool {
        self.value = -1.0
        true
    }

    method make_positive(mut self) {
        self.value = 1.0
    }
}

fn after_loop(raw Float, count Int) Result[Money, ConstraintError] {
    var value = raw
    for index in 0..count {
        value = 1.0
        Unit
    }
    Money(value)
}

fn after_impure_condition(raw Float) {
    var cell = Cell { value = raw }
    if cell.value >= 0.0 && cell.make_negative() {
        let checked = Money(cell.value)
        Unit
    } else {
        Unit
    }
}

fn copied_before_mutation(raw Float) {
    var cell = Cell { value = raw }
    let snapshot = cell.value
    cell.make_positive()
    if cell.value >= 0.0 {
        let checked = Money(snapshot)
        Unit
    } else {
        Unit
    }
}
",
        );
        assert!(
            analysis.diagnostics.is_empty(),
            "{:#?}",
            analysis.diagnostics
        );
        let checks = analysis
            .typed
            .bodies
            .iter()
            .flat_map(|(_, body)| body.construction_checks.values().copied())
            .collect::<Vec<_>>();
        assert_eq!(
            checks
                .iter()
                .filter(|check| **check == ConstructionCheck::Runtime)
                .count(),
            3,
            "{checks:?}"
        );
        assert!(
            checks
                .iter()
                .all(|check| *check == ConstructionCheck::Runtime),
            "{checks:?}"
        );
    }
}
