use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use loom_core::{Diagnostic, Severity, Span};
use loom_hir::{
    BinaryOp as HirBinaryOp, Body, BodyId, DefId, DefinitionKind, ExprId, GenericParamId, Literal,
    LocalId as HirLocalId, ParamId, PatternId, Program as HirProgram, ReceiverKind,
    Statement as HirStatement, UnaryOp as HirUnaryOp, Visibility,
};
use loom_mir::{
    AssociatedTypeDef, BinaryOp, Block, Builtin, CallArgument, CallPlan, CallTarget,
    CheckedProgram, ConceptDef, ConceptId, ConceptIdentity, Constant, ConstructionMode, Contract,
    ContractArm, ContractExpr, ContractExprKind, ContractValue, Expr, ExprKind, FieldDef, Function,
    FunctionId, LocalDecl, LocalId, MatchArm, Pattern, PreludeIds, Program, Receiver,
    ReceiverInvariantCheck, RequirementDef, RequirementId, RequirementType,
    RequirementWitnessParam, ScopedDisposal as MirScopedDisposal, Statement, StatementKind,
    SuspensionPoint, Type, TypeDef, TypeDefKind, TypeId, UnaryOp, VariantDef, VariantId, Witness,
    WitnessId, WitnessParam, WitnessRef,
};
use loom_sema::{
    Analysis, BodySemantics, BuiltinType, BuiltinValue, CallResolution,
    CallTarget as SemaCallTarget, Coercion, ConstantValue, ConstructionCheck, Mutability,
    Place as SemaPlace, PlaceProjection, PlaceRoot, Resolution, RuntimeCheck,
    ScopedDisposal as SemaScopedDisposal, Signature, TaskIntrinsic, TyData, TyId, ViewSource,
    WitnessSelection, WitnessSource,
};

const OPTION_TYPE: TypeId = TypeId(0);
const RESULT_TYPE: TypeId = TypeId(1);
const CONSTRAINT_ERROR_TYPE: TypeId = TypeId(2);
const CONTRACT_FAULT_TYPE: TypeId = TypeId(3);
const TASK_FAULT_TYPE: TypeId = TypeId(4);
const TASK_OUTCOME_TYPE: TypeId = TypeId(5);
const DURATION_TYPE: TypeId = TypeId(6);
const BYTES_TYPE: TypeId = TypeId(7);
const PATH_TYPE: TypeId = TypeId(8);
const TEXT_MAP_TYPE: TypeId = TypeId(9);
const JSON_TYPE: TypeId = TypeId(10);
const JSON_ERROR_TYPE: TypeId = TypeId(11);
const SYNTHETIC_TYPE_COUNT: u32 = 12;

/// Failure at the trusted typed-HIR to MIR boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoweringFailure {
    diagnostics: Vec<Diagnostic>,
}

impl LoweringFailure {
    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    #[must_use]
    pub fn into_diagnostics(self) -> Vec<Diagnostic> {
        self.diagnostics
    }
}

impl fmt::Display for LoweringFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "typed HIR to MIR lowering failed with {} compiler defect(s)",
            self.diagnostics.len()
        )
    }
}

impl Error for LoweringFailure {}

type LowerResult<T> = Result<T, Diagnostic>;

/// Lowers a complete, error-free semantic analysis into validated executable
/// MIR. No partially built program crosses this boundary.
///
/// # Errors
///
/// Returns structured `CompilerDefect` diagnostics if semantic facts are
/// missing, a checked source form has no MIR representation, or MIR validation
/// rejects the compiler's output.
pub fn lower_to_mir(
    hir: &HirProgram,
    analysis: &Analysis,
) -> Result<CheckedProgram, LoweringFailure> {
    if analysis.has_errors() {
        return Err(single_failure(defect(
            "MIR lowering requires an error-free semantic analysis",
            analysis
                .diagnostics
                .iter()
                .find(|diagnostic| diagnostic.severity == Severity::Error)
                .map_or_else(Span::default, |diagnostic| diagnostic.primary),
        )));
    }

    let compiler = Compiler::new(hir, analysis).map_err(single_failure)?;
    let program = compiler.run().map_err(single_failure)?;
    program.into_checked().map_err(|errors| LoweringFailure {
        diagnostics: errors
            .iter()
            .map(|error| {
                defect(
                    format!(
                        "compiler emitted invalid MIR at {}: {} ({})",
                        error.path, error.message, error.code
                    ),
                    error.span,
                )
            })
            .collect(),
    })
}

fn single_failure(diagnostic: Diagnostic) -> LoweringFailure {
    LoweringFailure {
        diagnostics: vec![diagnostic],
    }
}

fn defect(message: impl Into<String>, span: Span) -> Diagnostic {
    Diagnostic::error("CompilerDefect", message, span)
}

#[derive(Default)]
struct Indices {
    types: BTreeMap<DefId, TypeId>,
    functions: BTreeMap<DefId, FunctionId>,
    concepts: BTreeMap<DefId, ConceptId>,
    requirements: BTreeMap<DefId, RequirementId>,
    witnesses: BTreeMap<DefId, WitnessId>,
    fields: BTreeMap<DefId, u32>,
    variants: BTreeMap<DefId, (TypeId, VariantId)>,
}

struct Compiler<'a> {
    hir: &'a HirProgram,
    analysis: &'a Analysis,
    indices: Indices,
    file_definition: DefId,
    file_type: TypeId,
    io_error_definition: DefId,
    io_error_type: TypeId,
    io_error_kind_type: TypeId,
    socket_definition: DefId,
    socket_type: TypeId,
}

impl<'a> Compiler<'a> {
    #[allow(clippy::too_many_lines)]
    fn new(hir: &'a HirProgram, analysis: &'a Analysis) -> LowerResult<Self> {
        let mut indices = Indices::default();

        let mut next_type = SYNTHETIC_TYPE_COUNT;
        for (definition, source) in hir.definitions.iter() {
            if matches!(
                source.kind,
                DefinitionKind::RefinedType(_)
                    | DefinitionKind::Record(_)
                    | DefinitionKind::Enum(_)
            ) {
                indices.types.insert(definition, TypeId(next_type));
                next_type = next_type.checked_add(1).ok_or_else(|| {
                    defect(
                        "too many MIR type definitions",
                        definition_span(hir, definition),
                    )
                })?;
            }
        }

        let mut next_function = 0_u32;
        for (definition, source) in hir.definitions.iter() {
            if executable_body(&source.kind).is_some() {
                indices
                    .functions
                    .insert(definition, FunctionId(next_function));
                next_function = next_function.checked_add(1).ok_or_else(|| {
                    defect(
                        "too many MIR function definitions",
                        definition_span(hir, definition),
                    )
                })?;
            }
        }

        let mut next_concept = 0_u32;
        let mut next_requirement = 0_u32;
        for (definition, source) in hir.definitions.iter() {
            if let DefinitionKind::Concept(concept) = &source.kind {
                indices.concepts.insert(definition, ConceptId(next_concept));
                next_concept = next_concept.checked_add(1).ok_or_else(|| {
                    defect(
                        "too many MIR concept definitions",
                        definition_span(hir, definition),
                    )
                })?;
                for requirement in &concept.requirements {
                    indices
                        .requirements
                        .insert(*requirement, RequirementId(next_requirement));
                    next_requirement = next_requirement.checked_add(1).ok_or_else(|| {
                        defect(
                            "too many MIR concept requirements",
                            definition_span(hir, *requirement),
                        )
                    })?;
                }
            }
        }

        let mut next_witness = 0_u32;
        for (definition, source) in hir.definitions.iter() {
            if matches!(source.kind, DefinitionKind::Conformance(_))
                && analysis.typed.conformances.get(definition).is_some()
            {
                indices
                    .witnesses
                    .insert(definition, WitnessId(next_witness));
                next_witness = next_witness.checked_add(1).ok_or_else(|| {
                    defect("too many MIR witnesses", definition_span(hir, definition))
                })?;
            }
        }

        for (definition, source) in hir.definitions.iter() {
            match &source.kind {
                DefinitionKind::Record(record) => {
                    for (index, field) in record.fields.iter().copied().enumerate() {
                        indices.fields.insert(
                            field,
                            u32::try_from(index).map_err(|_| {
                                defect(
                                    "record has more than u32::MAX fields",
                                    definition_span(hir, definition),
                                )
                            })?,
                        );
                    }
                }
                DefinitionKind::Enum(enumeration) => {
                    let ty = required(
                        indices.types.get(&definition).copied(),
                        "enum has no assigned MIR type id",
                        definition_span(hir, definition),
                    )?;
                    for (index, variant) in enumeration.variants.iter().copied().enumerate() {
                        indices.variants.insert(
                            variant,
                            (
                                ty,
                                VariantId(u32::try_from(index).map_err(|_| {
                                    defect(
                                        "enum has more than u32::MAX variants",
                                        definition_span(hir, definition),
                                    )
                                })?),
                            ),
                        );
                    }
                }
                _ => {}
            }
        }

        let (file_definition, file_type) = canonical_resource_type(
            hir,
            &indices,
            analysis.canonical_std_items.file,
            "std.file.File",
        )?;
        let io_error_definition = required(
            analysis.canonical_std_items.io_error,
            "embedded std.io.IoError is required for MIR lowering",
            Span::default(),
        )?;
        let io_error_span = definition_span(hir, io_error_definition);
        let io_error_source = &hir.definitions[io_error_definition];
        let DefinitionKind::Record(io_error) = &io_error_source.kind else {
            return Err(defect(
                "canonical std.io.IoError must be an empty source record",
                io_error_span,
            ));
        };
        if io_error_source.visibility != Visibility::Public
            || !io_error.generic_params.is_empty()
            || !io_error.fields.is_empty()
            || io_error.invariant.is_some()
        {
            return Err(defect(
                "canonical std.io.IoError must be a public empty non-generic record without an invariant",
                io_error_span,
            ));
        }
        let io_error_type = required(
            indices.types.get(&io_error_definition).copied(),
            "canonical IoError has no MIR type id",
            io_error_span,
        )?;
        let io_error_kind_definition = required(
            analysis.canonical_std_items.io_error_kind,
            "embedded std.io.IoErrorKind is required for MIR lowering",
            Span::default(),
        )?;
        let io_error_kind_type = required(
            indices.types.get(&io_error_kind_definition).copied(),
            "canonical IoErrorKind has no MIR type id",
            definition_span(hir, io_error_kind_definition),
        )?;
        let (socket_definition, socket_type) = canonical_resource_type(
            hir,
            &indices,
            analysis.canonical_std_items.socket,
            "std.net.Socket",
        )?;

        Ok(Self {
            hir,
            analysis,
            indices,
            file_definition,
            file_type,
            io_error_definition,
            io_error_type,
            io_error_kind_type,
            socket_definition,
            socket_type,
        })
    }

    fn canonical_std_type_id(
        &self,
        definition: Option<DefId>,
        missing: &'static str,
    ) -> LowerResult<Option<TypeId>> {
        definition
            .map(|definition| {
                required(
                    self.indices.types.get(&definition).copied(),
                    missing,
                    definition_span(self.hir, definition),
                )
            })
            .transpose()
    }

    fn canonical_concept_id(
        &self,
        definition: Option<DefId>,
        missing: &'static str,
    ) -> LowerResult<Option<ConceptId>> {
        definition
            .map(|definition| {
                required(
                    self.indices.concepts.get(&definition).copied(),
                    missing,
                    definition_span(self.hir, definition),
                )
            })
            .transpose()
    }

    fn canonical_requirement_id(
        &self,
        definition: Option<DefId>,
        missing: &'static str,
    ) -> LowerResult<Option<RequirementId>> {
        definition
            .map(|definition| {
                required(
                    self.indices.requirements.get(&definition).copied(),
                    missing,
                    definition_span(self.hir, definition),
                )
            })
            .transpose()
    }

    fn run(&self) -> LowerResult<Program> {
        let dispose = self.canonical_concept_id(
            self.analysis.canonical_concepts.dispose,
            "canonical Dispose concept has no MIR id",
        )?;
        let must_scope = self.canonical_concept_id(
            self.analysis.canonical_concepts.must_scope,
            "canonical MustScope concept has no MIR id",
        )?;
        let dispose_requirement = self.canonical_requirement_id(
            self.analysis.canonical_concepts.dispose_requirement,
            "canonical Dispose.dispose requirement has no MIR id",
        )?;
        let no_suspend = self.canonical_concept_id(
            self.analysis.canonical_concepts.no_suspend,
            "canonical NoSuspend concept has no MIR id",
        )?;
        let decode_text_error = self.canonical_std_type_id(
            self.analysis.canonical_std_items.decode_text_error,
            "canonical DecodeTextError has no MIR type id",
        )?;
        let path_error = self.canonical_std_type_id(
            self.analysis.canonical_std_items.path_error,
            "canonical PathError has no MIR type id",
        )?;
        let log_level = self.canonical_std_type_id(
            self.analysis.canonical_std_items.log_level,
            "canonical LogLevel has no MIR type id",
        )?;
        let mut program = Program {
            types: self.lower_types()?,
            concepts: self.lower_concepts()?,
            requirements: self.lower_requirements()?,
            functions: self.lower_functions()?,
            witnesses: self.lower_witnesses()?,
            tests: self.lower_tests(),
            exports: self.lower_exports(),
            prelude: PreludeIds {
                result: Some(RESULT_TYPE),
                option: Some(OPTION_TYPE),
                constraint_error: Some(CONSTRAINT_ERROR_TYPE),
                task_fault: Some(TASK_FAULT_TYPE),
                task_outcome: Some(TASK_OUTCOME_TYPE),
                duration: Some(DURATION_TYPE),
                file: Some(self.file_type),
                socket: Some(self.socket_type),
                bytes: Some(BYTES_TYPE),
                path: Some(PATH_TYPE),
                decode_text_error,
                path_error,
                text_map: Some(TEXT_MAP_TYPE),
                json: Some(JSON_TYPE),
                json_error: Some(JSON_ERROR_TYPE),
                io_error: Some(self.io_error_type),
                io_error_kind: Some(self.io_error_kind_type),
                log_level,
                dispose_concept: dispose,
                dispose_requirement,
                must_scope_concept: must_scope,
                no_suspend_concept: no_suspend,
            },
        };
        program.types.shrink_to_fit();
        program.functions.shrink_to_fit();
        program.witnesses.shrink_to_fit();
        Ok(program)
    }

    #[allow(clippy::too_many_lines)]
    fn lower_types(&self) -> LowerResult<Vec<TypeDef>> {
        let mut types = synthetic_types();
        for (definition, source) in self.hir.definitions.iter() {
            let Some(id) = self.indices.types.get(&definition).copied() else {
                continue;
            };
            if definition == self.file_definition {
                if id.0 as usize != types.len() {
                    return Err(defect(
                        "MIR type id allocation is not dense",
                        definition_span(self.hir, definition),
                    ));
                }
                types.push(opaque_record_type(
                    id,
                    "File",
                    Type::Int,
                    definition_span(self.hir, definition),
                ));
                continue;
            }
            if definition == self.io_error_definition {
                if id.0 as usize != types.len() {
                    return Err(defect(
                        "MIR type id allocation is not dense",
                        definition_span(self.hir, definition),
                    ));
                }
                types.push(io_error_type(
                    id,
                    self.io_error_kind_type,
                    definition_span(self.hir, definition),
                ));
                continue;
            }
            if definition == self.socket_definition {
                if id.0 as usize != types.len() {
                    return Err(defect(
                        "MIR type id allocation is not dense",
                        definition_span(self.hir, definition),
                    ));
                }
                types.push(opaque_record_type(
                    id,
                    "Socket",
                    Type::Int,
                    definition_span(self.hir, definition),
                ));
                continue;
            }
            let name = definition_name(self.hir, definition)?;
            let span = definition_span(self.hir, definition);
            let type_parameters = match &source.kind {
                DefinitionKind::Record(record) => u32::try_from(record.generic_params.len())
                    .map_err(|_| defect("too many record type parameters", span))?,
                DefinitionKind::Enum(enumeration) => {
                    u32::try_from(enumeration.generic_params.len())
                        .map_err(|_| defect("too many enum type parameters", span))?
                }
                _ => 0,
            };
            let kind = match &source.kind {
                DefinitionKind::RefinedType(refined) => {
                    let base = self.resolved_type_ref(refined.base)?;
                    TypeDefKind::Refined {
                        base: self.lower_ty(base, &TypeParameters::default(), span)?,
                        predicate: self.lower_contract(refined.predicate, "constraint")?,
                    }
                }
                DefinitionKind::Record(record) => {
                    let parameters = TypeParameters::from_ids(&record.generic_params)?;
                    let mut fields = Vec::with_capacity(record.fields.len());
                    for field in &record.fields {
                        let Signature::Field { ty, .. } = self.signature(*field)? else {
                            return Err(defect(
                                "record member is missing a field signature",
                                definition_span(self.hir, *field),
                            ));
                        };
                        fields.push(FieldDef {
                            name: definition_name(self.hir, *field)?,
                            ty: self.lower_ty(
                                *ty,
                                &parameters,
                                definition_span(self.hir, *field),
                            )?,
                            span: definition_span(self.hir, *field),
                        });
                    }
                    TypeDefKind::Record {
                        fields,
                        invariant: record
                            .invariant
                            .map(|body| self.lower_contract(body, "invariant"))
                            .transpose()?,
                    }
                }
                DefinitionKind::Enum(enumeration) => {
                    let parameters = TypeParameters::from_ids(&enumeration.generic_params)?;
                    let mut variants = Vec::with_capacity(enumeration.variants.len());
                    for variant in &enumeration.variants {
                        let Signature::Variant { payload, .. } = self.signature(*variant)? else {
                            return Err(defect(
                                "enum member is missing a variant signature",
                                definition_span(self.hir, *variant),
                            ));
                        };
                        let (_, id) = required(
                            self.indices.variants.get(variant).copied(),
                            "enum variant has no assigned MIR id",
                            definition_span(self.hir, *variant),
                        )?;
                        variants.push(VariantDef {
                            id,
                            name: definition_name(self.hir, *variant)?,
                            payload: payload
                                .iter()
                                .map(|ty| {
                                    self.lower_ty(
                                        *ty,
                                        &parameters,
                                        definition_span(self.hir, *variant),
                                    )
                                })
                                .collect::<LowerResult<_>>()?,
                            span: definition_span(self.hir, *variant),
                        });
                    }
                    TypeDefKind::Enum { variants }
                }
                _ => {
                    return Err(defect("non-type definition received a MIR type id", span));
                }
            };
            if id.0 as usize != types.len() {
                return Err(defect("MIR type id allocation is not dense", span));
            }
            types.push(TypeDef {
                id,
                name,
                span,
                type_parameters,
                kind,
            });
        }
        Ok(types)
    }

    fn signature(&self, definition: DefId) -> LowerResult<&Signature> {
        required(
            self.analysis.typed.signatures.get(definition),
            "definition has no semantic signature",
            definition_span(self.hir, definition),
        )
    }

    fn resolved_type_ref(&self, ty: loom_hir::TypeRefId) -> LowerResult<TyId> {
        required(
            self.analysis.typed.resolved_type_refs.get(ty).copied(),
            "type reference has no resolved semantic type",
            self.hir.source_map.type_ref(ty).unwrap_or_default(),
        )
    }

    fn lower_ty(&self, ty: TyId, parameters: &TypeParameters, span: Span) -> LowerResult<Type> {
        match self.analysis.typed.types.data(ty) {
            TyData::Error => Err(defect("recovery Error type reached MIR lowering", span)),
            TyData::Never => Ok(Type::Never),
            TyData::Builtin(builtin) => Ok(lower_builtin_type(*builtin)),
            TyData::Tuple(elements) => Ok(Type::Tuple(
                elements
                    .iter()
                    .map(|element| self.lower_ty(*element, parameters, span))
                    .collect::<LowerResult<_>>()?,
            )),
            TyData::List(element) => Ok(Type::List(Box::new(
                self.lower_ty(*element, parameters, span)?,
            ))),
            TyData::TextMap(value) => Ok(Type::Nominal(
                TEXT_MAP_TYPE,
                vec![self.lower_ty(*value, parameters, span)?],
            )),
            TyData::Option(element) => Ok(Type::Nominal(
                OPTION_TYPE,
                vec![self.lower_ty(*element, parameters, span)?],
            )),
            TyData::Result { ok, error } => Ok(Type::Nominal(
                RESULT_TYPE,
                vec![
                    self.lower_ty(*ok, parameters, span)?,
                    self.lower_ty(*error, parameters, span)?,
                ],
            )),
            TyData::Task(output) => Ok(Type::Task(Box::new(
                self.lower_ty(*output, parameters, span)?,
            ))),
            TyData::TaskOutcome(output) => Ok(Type::Nominal(
                TASK_OUTCOME_TYPE,
                vec![self.lower_ty(*output, parameters, span)?],
            )),
            TyData::Nominal {
                definition,
                arguments,
            } => Ok(Type::Nominal(
                required(
                    self.indices.types.get(definition).copied(),
                    "nominal semantic type has no MIR definition",
                    span,
                )?,
                arguments
                    .iter()
                    .map(|argument| self.lower_ty(*argument, parameters, span))
                    .collect::<LowerResult<_>>()?,
            )),
            TyData::Param(parameter) => parameters.parameter_type(*parameter, span),
            TyData::View { mutability, target } => {
                let TyData::DynTarget(instance) = self.analysis.typed.types.data(*target) else {
                    return Err(defect(
                        "interface target is not a dyn concept instance",
                        span,
                    ));
                };
                let concept = required(
                    self.indices.concepts.get(&instance.concept).copied(),
                    "dyn target concept has no MIR id",
                    span,
                )?;
                let mut bindings = BTreeMap::new();
                for binding in &instance.bindings {
                    let name = definition_name(self.hir, binding.associated_type)?;
                    bindings.insert(name, self.lower_ty(binding.ty, parameters, span)?);
                }
                Ok(Type::View {
                    mutable: *mutability == Mutability::Mutable,
                    concept,
                    bindings,
                })
            }
            TyData::SelfType(concept) => parameters.self_type(*concept, span),
            TyData::Projection {
                self_ty,
                concept,
                associated_type,
            } => {
                if matches!(self.analysis.typed.types.data(*self_ty), TyData::SelfType(owner) if owner == concept)
                    && let Some(ty) = parameters.associated_type(*concept, *associated_type)
                {
                    return Ok(ty);
                }
                Ok(Type::AssociatedProjection {
                    witness: parameters.projection_witness(*self_ty, *concept, span)?,
                    associated: definition_name(self.hir, *associated_type)?,
                })
            }
            TyData::DynTarget(_) => Err(defect(
                "bare dyn target reached executable MIR type lowering",
                span,
            )),
        }
    }

    fn lower_tests(&self) -> Vec<FunctionId> {
        self.hir
            .definitions
            .iter()
            .filter_map(|(definition, source)| {
                (matches!(source.kind, DefinitionKind::Test(_))
                    && self.hir.is_root_test_companion(source.module))
                .then(|| self.indices.functions.get(&definition).copied())
                .flatten()
            })
            .collect()
    }

    fn lower_witness_prefix_count(
        &self,
        kind: &DefinitionKind,
        signature: &loom_sema::CallableSignature,
        span: Span,
    ) -> LowerResult<u32> {
        let DefinitionKind::Method(method) = kind else {
            return match kind {
                DefinitionKind::Function(_) | DefinitionKind::Test(_) => Ok(0),
                _ => Err(defect(
                    "non-callable definition received a MIR function id",
                    span,
                )),
            };
        };
        if !matches!(
            self.hir.definitions[method.owner].kind,
            DefinitionKind::Conformance(_)
        ) {
            return Ok(0);
        }
        let prefix = signature
            .bounds
            .len()
            .checked_sub(signature.call_bounds.len())
            .ok_or_else(|| defect("call-bound metadata exceeds all callable bounds", span))?;
        u32::try_from(prefix).map_err(|_| defect("too many conformance witness parameters", span))
    }

    fn lower_functions(&self) -> LowerResult<Vec<Function>> {
        let mut functions = Vec::with_capacity(self.indices.functions.len());
        for (definition, source) in self.hir.definitions.iter() {
            let Some(id) = self.indices.functions.get(&definition).copied() else {
                continue;
            };
            let body_id = required(
                executable_body(&source.kind),
                "executable definition has no HIR body",
                definition_span(self.hir, definition),
            )?;
            let Signature::Callable(signature) = self.signature(definition)? else {
                return Err(defect(
                    "executable definition has no callable semantic signature",
                    definition_span(self.hir, definition),
                ));
            };
            let parameters = TypeParameters::from_callable(signature)?;
            let body = &self.hir.bodies[body_id];
            let semantics = required(
                self.analysis.typed.body(body_id),
                "executable body has no semantic side table",
                self.hir.source_map.body(body_id).unwrap_or_default(),
            )?;
            let receiver_ty = self.receiver_type(definition)?;
            let mut builder = FunctionLowerer::new(
                self,
                definition,
                body,
                semantics,
                parameters,
                signature,
                receiver_ty,
            )?;
            let mir_body = builder.lower_root()?;
            let receiver = match signature.receiver {
                Some(ReceiverKind::ReadOnly) => Some(Receiver::Readonly),
                Some(ReceiverKind::Mutable) => Some(Receiver::Mutable),
                Some(ReceiverKind::Static) | None => None,
            };
            let call_plan = self.lower_call_plan(definition)?;
            let (params, locals, suspension_points) =
                builder.finish_locals(&mir_body, receiver, &call_plan)?;
            let type_parameters = builder.parameters.len();
            let name = self.qualified_definition_name(definition)?;
            let span = definition_span(self.hir, definition);
            if id.0 as usize != functions.len() {
                return Err(defect("MIR function id allocation is not dense", span));
            }
            let witness_prefix_count =
                self.lower_witness_prefix_count(&source.kind, signature, span)?;
            let mut function = Function {
                id,
                name,
                span,
                type_parameters,
                is_async: signature.is_async,
                suspension_points,
                params,
                witness_params: signature
                    .bounds
                    .iter()
                    .map(|bound| {
                        self.lower_witness_param(
                            bound.self_ty,
                            &bound.concept,
                            &builder.parameters,
                            span,
                        )
                    })
                    .collect::<LowerResult<_>>()?,
                witness_prefix_count,
                locals,
                return_ty: self.lower_ty(signature.return_ty, &builder.parameters, span)?,
                receiver,
                body: mir_body,
                call_plan,
            };
            function.renumber_expr_ids().map_err(|error| {
                defect(
                    format!("could not assign MIR expression ids: {error}"),
                    span,
                )
            })?;
            functions.push(function);
        }
        Ok(functions)
    }

    fn lower_concepts(&self) -> LowerResult<Vec<ConceptDef>> {
        let mut concepts = Vec::with_capacity(self.indices.concepts.len());
        for (definition, source) in self.hir.definitions.iter() {
            let Some(id) = self.indices.concepts.get(&definition).copied() else {
                continue;
            };
            let DefinitionKind::Concept(concept) = &source.kind else {
                return Err(defect(
                    "non-concept definition received a MIR concept id",
                    definition_span(self.hir, definition),
                ));
            };
            let span = definition_span(self.hir, definition);
            if id.0 as usize != concepts.len() {
                return Err(defect("MIR concept id allocation is not dense", span));
            }
            concepts.push(ConceptDef {
                id,
                module: self.hir.modules[self.hir.production_module(source.module)]
                    .name
                    .to_string(),
                name: definition_name(self.hir, definition)?,
                span,
                identity: if self.analysis.canonical_concepts.dispose == Some(definition) {
                    Some(ConceptIdentity::Dispose)
                } else if self.analysis.canonical_concepts.must_scope == Some(definition) {
                    Some(ConceptIdentity::MustScope)
                } else if self.analysis.canonical_concepts.no_suspend == Some(definition) {
                    Some(ConceptIdentity::NoSuspend)
                } else {
                    None
                },
                dynamic: concept.dyn_capable,
                associated_types: concept
                    .associated_types
                    .iter()
                    .map(|associated| {
                        Ok(AssociatedTypeDef {
                            name: definition_name(self.hir, *associated)?,
                            span: definition_span(self.hir, *associated),
                        })
                    })
                    .collect::<LowerResult<_>>()?,
                requirements: concept
                    .requirements
                    .iter()
                    .map(|requirement| {
                        required(
                            self.indices.requirements.get(requirement).copied(),
                            "concept requirement has no MIR id",
                            span,
                        )
                    })
                    .collect::<LowerResult<_>>()?,
            });
        }
        Ok(concepts)
    }

    #[allow(clippy::too_many_lines)]
    fn lower_requirements(&self) -> LowerResult<Vec<RequirementDef>> {
        let mut requirements = Vec::with_capacity(self.indices.requirements.len());
        for (definition, id) in &self.indices.requirements {
            let DefinitionKind::Method(method) = &self.hir.definitions[*definition].kind else {
                return Err(defect(
                    "non-method definition received a requirement id",
                    definition_span(self.hir, *definition),
                ));
            };
            let Signature::Callable(signature) = self.signature(*definition)? else {
                return Err(defect(
                    "concept requirement has no callable signature",
                    definition_span(self.hir, *definition),
                ));
            };
            let span = definition_span(self.hir, *definition);
            let mut parameters = TypeParameters::from_ids(&signature.call_generic_params)?;
            for (index, bound) in signature.call_bounds.iter().enumerate() {
                parameters.projection_witnesses.insert(
                    (bound.self_ty, bound.concept.concept),
                    u32::try_from(index)
                        .map_err(|_| defect("too many requirement witness parameters", span))?,
                );
            }
            let mut params = Vec::with_capacity(
                signature.params.len()
                    + usize::from(signature.receiver != Some(ReceiverKind::Static)),
            );
            if matches!(
                signature.receiver,
                Some(ReceiverKind::ReadOnly | ReceiverKind::Mutable)
            ) {
                params.push(RequirementType::SelfType);
            }
            params.extend(
                signature
                    .params
                    .iter()
                    .map(|(_, ty)| self.lower_requirement_ty(*ty, method.owner, &parameters, span))
                    .collect::<LowerResult<Vec<_>>>()?,
            );
            if id.0 as usize != requirements.len() {
                return Err(defect("MIR requirement id allocation is not dense", span));
            }
            requirements.push(RequirementDef {
                id: *id,
                concept: required(
                    self.indices.concepts.get(&method.owner).copied(),
                    "requirement owner concept has no MIR id",
                    span,
                )?,
                name: definition_name(self.hir, *definition)?,
                span,
                receiver: match signature.receiver {
                    Some(ReceiverKind::ReadOnly) => Some(Receiver::Readonly),
                    Some(ReceiverKind::Mutable) => Some(Receiver::Mutable),
                    Some(ReceiverKind::Static) | None => None,
                },
                method_type_parameters: parameters.len(),
                params,
                return_ty: self.lower_requirement_ty(
                    signature.return_ty,
                    method.owner,
                    &parameters,
                    span,
                )?,
                witness_params: signature
                    .call_bounds
                    .iter()
                    .map(|bound| {
                        Ok(RequirementWitnessParam {
                            target: self.lower_requirement_ty(
                                bound.self_ty,
                                method.owner,
                                &parameters,
                                span,
                            )?,
                            concept: required(
                                self.indices.concepts.get(&bound.concept.concept).copied(),
                                "requirement bound concept has no MIR id",
                                span,
                            )?,
                            bindings: bound
                                .concept
                                .bindings
                                .iter()
                                .map(|binding| {
                                    Ok((
                                        definition_name(self.hir, binding.associated_type)?,
                                        self.lower_requirement_ty(
                                            binding.ty,
                                            method.owner,
                                            &parameters,
                                            span,
                                        )?,
                                    ))
                                })
                                .collect::<LowerResult<_>>()?,
                            span,
                        })
                    })
                    .collect::<LowerResult<_>>()?,
            });
        }
        Ok(requirements)
    }

    #[allow(clippy::too_many_lines)]
    fn lower_requirement_ty(
        &self,
        ty: TyId,
        owner_concept: DefId,
        parameters: &TypeParameters,
        span: Span,
    ) -> LowerResult<RequirementType> {
        match self.analysis.typed.types.data(ty) {
            TyData::Error | TyData::Never => Err(defect(
                "non-runtime type reached concept requirement metadata",
                span,
            )),
            TyData::Builtin(builtin) => Ok(match builtin {
                BuiltinType::Bool => RequirementType::Bool,
                BuiltinType::Int => RequirementType::Int,
                BuiltinType::Float => RequirementType::Float,
                BuiltinType::Text => RequirementType::Text,
                BuiltinType::Bytes => RequirementType::Nominal(BYTES_TYPE, Vec::new()),
                BuiltinType::Path => RequirementType::Nominal(PATH_TYPE, Vec::new()),
                BuiltinType::Unit => RequirementType::Unit,
                BuiltinType::ConstraintError => {
                    RequirementType::Nominal(CONSTRAINT_ERROR_TYPE, Vec::new())
                }
                BuiltinType::ContractFault => {
                    RequirementType::Nominal(CONTRACT_FAULT_TYPE, Vec::new())
                }
                BuiltinType::TaskFault => RequirementType::Nominal(TASK_FAULT_TYPE, Vec::new()),
                BuiltinType::Duration => RequirementType::Nominal(DURATION_TYPE, Vec::new()),
                BuiltinType::Json => RequirementType::Nominal(JSON_TYPE, Vec::new()),
                BuiltinType::JsonError => RequirementType::Nominal(JSON_ERROR_TYPE, Vec::new()),
            }),
            TyData::Tuple(elements) => Ok(RequirementType::Tuple(
                elements
                    .iter()
                    .map(|element| {
                        self.lower_requirement_ty(*element, owner_concept, parameters, span)
                    })
                    .collect::<LowerResult<_>>()?,
            )),
            TyData::Option(element) => Ok(RequirementType::Nominal(
                OPTION_TYPE,
                vec![self.lower_requirement_ty(*element, owner_concept, parameters, span)?],
            )),
            TyData::TextMap(value) => Ok(RequirementType::Nominal(
                TEXT_MAP_TYPE,
                vec![self.lower_requirement_ty(*value, owner_concept, parameters, span)?],
            )),
            TyData::Result { ok, error } => Ok(RequirementType::Nominal(
                RESULT_TYPE,
                vec![
                    self.lower_requirement_ty(*ok, owner_concept, parameters, span)?,
                    self.lower_requirement_ty(*error, owner_concept, parameters, span)?,
                ],
            )),
            TyData::Nominal {
                definition,
                arguments,
            } => Ok(RequirementType::Nominal(
                required(
                    self.indices.types.get(definition).copied(),
                    "requirement nominal type has no MIR definition",
                    span,
                )?,
                arguments
                    .iter()
                    .map(|argument| {
                        self.lower_requirement_ty(*argument, owner_concept, parameters, span)
                    })
                    .collect::<LowerResult<_>>()?,
            )),
            TyData::Param(parameter) => Ok(RequirementType::MethodParameter(
                parameters.index(*parameter, span)?,
            )),
            TyData::SelfType(concept) if *concept == owner_concept => Ok(RequirementType::SelfType),
            TyData::Projection {
                self_ty,
                concept,
                associated_type,
            } if *concept == owner_concept
                && matches!(
                    self.analysis.typed.types.data(*self_ty),
                    TyData::SelfType(owner) if *owner == owner_concept
                ) =>
            {
                Ok(RequirementType::Associated(definition_name(
                    self.hir,
                    *associated_type,
                )?))
            }
            TyData::Projection {
                self_ty,
                concept,
                associated_type,
            } => Ok(RequirementType::AssociatedProjection {
                witness: parameters.projection_witness(*self_ty, *concept, span)?,
                associated: definition_name(self.hir, *associated_type)?,
            }),
            TyData::View { mutability, target } => {
                let TyData::DynTarget(instance) = self.analysis.typed.types.data(*target) else {
                    return Err(defect("requirement interface target is not dyn", span));
                };
                Ok(RequirementType::View {
                    mutable: *mutability == Mutability::Mutable,
                    concept: required(
                        self.indices.concepts.get(&instance.concept).copied(),
                        "requirement dyn concept has no MIR id",
                        span,
                    )?,
                    bindings: instance
                        .bindings
                        .iter()
                        .map(|binding| {
                            Ok((
                                definition_name(self.hir, binding.associated_type)?,
                                self.lower_requirement_ty(
                                    binding.ty,
                                    owner_concept,
                                    parameters,
                                    span,
                                )?,
                            ))
                        })
                        .collect::<LowerResult<_>>()?,
                })
            }
            TyData::List(_)
            | TyData::Task(_)
            | TyData::TaskOutcome(_)
            | TyData::SelfType(_)
            | TyData::DynTarget(_) => Err(defect(
                "unresolved requirement type reached MIR metadata lowering",
                span,
            )),
        }
    }

    fn lower_witness_param(
        &self,
        target: TyId,
        concept: &loom_sema::ConceptInstance,
        parameters: &TypeParameters,
        span: Span,
    ) -> LowerResult<WitnessParam> {
        Ok(WitnessParam {
            target: self.lower_ty(target, parameters, span)?,
            concept: required(
                self.indices.concepts.get(&concept.concept).copied(),
                "witness parameter concept has no MIR id",
                span,
            )?,
            bindings: concept
                .bindings
                .iter()
                .map(|binding| {
                    Ok((
                        definition_name(self.hir, binding.associated_type)?,
                        self.lower_ty(binding.ty, parameters, span)?,
                    ))
                })
                .collect::<LowerResult<_>>()?,
            span,
        })
    }

    #[allow(clippy::too_many_lines)]
    fn lower_witnesses(&self) -> LowerResult<Vec<Witness>> {
        let mut witnesses = Vec::with_capacity(self.indices.witnesses.len());
        for (definition, source) in self.hir.definitions.iter() {
            let Some(id) = self.indices.witnesses.get(&definition).copied() else {
                continue;
            };
            let DefinitionKind::Conformance(conformance) = &source.kind else {
                return Err(defect(
                    "non-conformance definition received a witness id",
                    definition_span(self.hir, definition),
                ));
            };
            let semantics = required(
                self.analysis.typed.conformances.get(definition),
                "conformance has no semantic witness facts",
                definition_span(self.hir, definition),
            )?;
            let parameters = TypeParameters::from_ids(&conformance.generic_params)?;
            let span = definition_span(self.hir, definition);
            let mut associated = BTreeMap::new();
            for binding in &semantics.associated_types {
                associated.insert(
                    definition_name(self.hir, binding.associated_type)?,
                    self.lower_ty(binding.ty, &parameters, span)?,
                );
            }
            let methods = semantics
                .methods
                .iter()
                .map(|(requirement, implementation)| {
                    Ok((
                        required(
                            self.indices.requirements.get(requirement).copied(),
                            "conformance requirement has no MIR id",
                            span,
                        )?,
                        required(
                            self.indices.functions.get(implementation).copied(),
                            "conformance method has no MIR function",
                            span,
                        )?,
                    ))
                })
                .collect::<LowerResult<_>>()?;

            let header = self
                .analysis
                .impl_index
                .for_concept(semantics.concept.concept)
                .iter()
                .find(|header| header.definition == definition)
                .ok_or_else(|| defect("conformance has no accepted impl header", span))?;
            let condition_spans = conformance
                .generic_params
                .iter()
                .flat_map(|parameter| {
                    std::iter::repeat_n(
                        self.hir
                            .source_map
                            .generic_param(*parameter)
                            .unwrap_or(span),
                        self.hir.generic_params[*parameter].bounds.len(),
                    )
                })
                .collect::<Vec<_>>();
            if condition_spans.len() != header.conditions.len() {
                return Err(defect(
                    "conformance prerequisite spans and semantic goals disagree",
                    span,
                ));
            }
            if id.0 as usize != witnesses.len() {
                return Err(defect("MIR witness id allocation is not dense", span));
            }
            witnesses.push(Witness {
                id,
                concept: required(
                    self.indices
                        .concepts
                        .get(&semantics.concept.concept)
                        .copied(),
                    "conformance concept has no MIR id",
                    span,
                )?,
                concrete: self.lower_ty(semantics.target, &parameters, span)?,
                methods,
                associated,
                type_parameters: parameters.len(),
                prerequisites: header
                    .conditions
                    .iter()
                    .zip(condition_spans)
                    .map(|(condition, condition_span)| {
                        self.lower_witness_param(
                            condition.self_ty,
                            &condition.concept,
                            &parameters,
                            condition_span,
                        )
                    })
                    .collect::<LowerResult<_>>()?,
            });
        }
        Ok(witnesses)
    }

    fn receiver_type(&self, method: DefId) -> LowerResult<Option<TyId>> {
        let DefinitionKind::Method(source) = &self.hir.definitions[method].kind else {
            return Ok(None);
        };
        if source.signature.receiver == Some(ReceiverKind::Static) {
            return Ok(None);
        }
        let owner = source.owner;
        match &self.hir.definitions[owner].kind {
            DefinitionKind::InherentImpl(implementation) => {
                self.resolved_type_ref(implementation.target).map(Some)
            }
            DefinitionKind::Conformance(_) => required(
                self.analysis
                    .typed
                    .conformances
                    .get(owner)
                    .map(|semantics| semantics.target),
                "conformance method has no concrete receiver type",
                definition_span(self.hir, method),
            )
            .map(Some),
            DefinitionKind::Concept(_) => Ok(None),
            _ => Err(defect(
                "method owner is not an impl or concept",
                definition_span(self.hir, method),
            )),
        }
    }

    fn conformance_contract_parameters(
        &self,
        requirement: DefId,
        semantics: &loom_sema::ConformanceSemantics,
        implementation: &loom_sema::CallableSignature,
        mut parameters: TypeParameters,
        span: Span,
    ) -> LowerResult<TypeParameters> {
        let Signature::Callable(required_signature) = self.signature(requirement)? else {
            return Err(defect(
                "concept contract owner has no callable signature",
                span,
            ));
        };
        if required_signature.call_generic_params.len() != implementation.call_generic_params.len()
            || required_signature.call_bounds.len() != implementation.call_bounds.len()
        {
            return Err(defect(
                "accepted conformance has incompatible contract generic metadata",
                span,
            ));
        }
        for (required, actual) in required_signature
            .call_generic_params
            .iter()
            .zip(&implementation.call_generic_params)
        {
            parameters
                .substitutions
                .insert(*required, parameters.parameter_type(*actual, span)?);
        }

        let concrete_self = self.lower_ty(semantics.target, &parameters, span)?;
        parameters
            .self_types
            .insert(semantics.concept.concept, concrete_self);
        for binding in &semantics.associated_types {
            parameters.associated_types.insert(
                (semantics.concept.concept, binding.associated_type),
                self.lower_ty(binding.ty, &parameters, span)?,
            );
        }

        let proof_offset = implementation
            .bounds
            .len()
            .checked_sub(implementation.call_bounds.len())
            .ok_or_else(|| defect("call-bound metadata exceeds all callable bounds", span))?;
        for (index, bound) in required_signature.call_bounds.iter().enumerate() {
            parameters.projection_witnesses.insert(
                (bound.self_ty, bound.concept.concept),
                u32::try_from(proof_offset + index)
                    .map_err(|_| defect("too many callable witness parameters", span))?,
            );
        }
        Ok(parameters)
    }

    fn invariant_contract_parameters(
        &self,
        target: TyId,
        parameters: &TypeParameters,
        span: Span,
    ) -> LowerResult<TypeParameters> {
        let TyData::Nominal {
            definition,
            arguments,
        } = self.analysis.typed.types.data(target)
        else {
            return Ok(parameters.clone());
        };
        let DefinitionKind::Record(record) = &self.hir.definitions[*definition].kind else {
            return Ok(parameters.clone());
        };
        if record.generic_params.len() != arguments.len() {
            return Err(defect(
                "record invariant target has inconsistent generic arity",
                span,
            ));
        }
        let mut instantiated = parameters.clone();
        for (parameter, argument) in record.generic_params.iter().zip(arguments) {
            instantiated
                .substitutions
                .insert(*parameter, self.lower_ty(*argument, parameters, span)?);
        }
        Ok(instantiated)
    }

    #[allow(clippy::too_many_lines)]
    fn lower_call_plan(&self, definition: DefId) -> LowerResult<CallPlan> {
        let Signature::Callable(implementation_signature) = self.signature(definition)? else {
            return Err(defect(
                "call-plan owner has no callable signature",
                definition_span(self.hir, definition),
            ));
        };
        let function_parameters = TypeParameters::from_callable(implementation_signature)?;
        let span = definition_span(self.hir, definition);
        let (contract_owner, target, contract_parameters) =
            match &self.hir.definitions[definition].kind {
                DefinitionKind::Function(_) | DefinitionKind::Test(_) => {
                    (definition, None, function_parameters.clone())
                }
                DefinitionKind::Method(method) => {
                    match &self.hir.definitions[method.owner].kind {
                        DefinitionKind::Conformance(_) => {
                            let semantics = required(
                                self.analysis.typed.conformances.get(method.owner),
                                "conformance method has no semantic conformance",
                                definition_span(self.hir, definition),
                            )?;
                            let requirement = semantics.methods.iter().find_map(
                                |(requirement, implementation)| {
                                    (*implementation == definition).then_some(*requirement)
                                },
                            );
                            let requirement = required(
                                requirement,
                                "conformance method is not mapped to a requirement",
                                span,
                            )?;
                            (
                                requirement,
                                Some(semantics.target),
                                self.conformance_contract_parameters(
                                    requirement,
                                    semantics,
                                    implementation_signature,
                                    function_parameters.clone(),
                                    span,
                                )?,
                            )
                        }
                        DefinitionKind::InherentImpl(implementation) => (
                            definition,
                            Some(self.resolved_type_ref(implementation.target)?),
                            function_parameters.clone(),
                        ),
                        _ => (definition, None, function_parameters.clone()),
                    }
                }
                _ => {
                    return Err(defect(
                        "non-callable definition reached call-plan lowering",
                        definition_span(self.hir, definition),
                    ));
                }
            };
        let source = callable_source(self.hir, contract_owner)?;
        let receiver_invariant_body = if matches!(
            implementation_signature.receiver,
            Some(ReceiverKind::ReadOnly | ReceiverKind::Mutable)
        ) {
            let target = required(
                target,
                "receiver method call plan has no receiver target type",
                span,
            )?;
            match self.analysis.typed.types.data(target) {
                TyData::Nominal { definition, .. } => {
                    if let DefinitionKind::Record(record) = &self.hir.definitions[*definition].kind
                    {
                        record.invariant
                    } else {
                        None
                    }
                }
                _ => None,
            }
        } else {
            None
        };
        let receiver_invariant = if let Some(body) = receiver_invariant_body
            && !self.contract_is_proven(body)?
        {
            let target = required(
                target,
                "receiver invariant has no receiver target type",
                span,
            )?;
            let parameters =
                self.invariant_contract_parameters(target, &function_parameters, span)?;
            Some(self.lower_contract_with_parameters(body, "invariant", parameters)?)
        } else {
            None
        };
        let mut requires = Vec::new();
        for (index, body) in source.contracts.requires.iter().enumerate() {
            if !self.contract_is_proven(*body)? {
                requires.push(self.lower_contract_with_parameters(
                    *body,
                    &format!("requires[{index}]"),
                    contract_parameters.clone(),
                )?);
            }
        }
        let mut ensures = Vec::new();
        for (index, body) in source.contracts.ensures.iter().enumerate() {
            if !self.contract_is_proven(*body)? {
                ensures.push(self.lower_contract_with_parameters(
                    *body,
                    &format!("ensures[{index}]"),
                    contract_parameters.clone(),
                )?);
            }
        }
        Ok(CallPlan {
            receiver_invariant,
            requires,
            ensures,
        })
    }

    fn qualified_definition_name(&self, definition: DefId) -> LowerResult<String> {
        let source = &self.hir.definitions[definition];
        let module = &self.hir.modules[self.hir.production_module(source.module)].name;
        let name = definition_name(self.hir, definition)?;
        Ok(format!("{module}.{name}"))
    }

    fn lower_exports(&self) -> BTreeMap<String, FunctionId> {
        let mut exports = BTreeMap::new();
        let mut simple = BTreeMap::<String, Option<FunctionId>>::new();
        for (definition, source) in self.hir.definitions.iter() {
            if source.visibility != Visibility::Public
                || !matches!(source.kind, DefinitionKind::Function(_))
                || !self.hir.is_root_production_module(source.module)
            {
                continue;
            }
            let Some(id) = self.indices.functions.get(&definition).copied() else {
                continue;
            };
            let Some(name) = source.name.as_ref().map(ToString::to_string) else {
                continue;
            };
            let module = &self.hir.modules[self.hir.production_module(source.module)].name;
            exports.insert(format!("{module}.{name}"), id);
            simple
                .entry(name)
                .and_modify(|candidate| *candidate = None)
                .or_insert(Some(id));
        }
        for (name, id) in simple {
            if let Some(id) = id {
                exports.insert(name, id);
            }
        }
        exports
    }

    fn lower_contract(&self, body_id: BodyId, kind: &str) -> LowerResult<Contract> {
        let parameters = body_type_parameters(
            self.hir,
            &self.analysis.typed,
            self.hir.bodies[body_id].owner,
        )?;
        self.lower_contract_with_parameters(body_id, kind, parameters)
    }

    fn contract_is_proven(&self, body_id: BodyId) -> LowerResult<bool> {
        let semantics = required(
            self.analysis.typed.body(body_id),
            "contract body has no semantic side table",
            self.hir.source_map.body(body_id).unwrap_or_default(),
        )?;
        let check = required(
            semantics.contract_check,
            "contract body has no proof disposition",
            self.hir.source_map.body(body_id).unwrap_or_default(),
        )?;
        Ok(check == RuntimeCheck::Proven)
    }

    fn lower_contract_with_parameters(
        &self,
        body_id: BodyId,
        kind: &str,
        parameters: TypeParameters,
    ) -> LowerResult<Contract> {
        let body = &self.hir.bodies[body_id];
        let semantics = required(
            self.analysis.typed.body(body_id),
            "contract body has no semantic side table",
            self.hir.source_map.body(body_id).unwrap_or_default(),
        )?;
        let params = contract_parameter_indices(self.hir, body.owner)?;
        let expression = ContractLowerer {
            compiler: self,
            body,
            semantics,
            params,
            parameters,
        }
        .lower_root()?;
        let owner = definition_name(self.hir, body.owner)?;
        Ok(Contract {
            code: format!("{owner}.{kind}"),
            span: self.hir.source_map.body(body_id).unwrap_or_default(),
            expression,
        })
    }
}

struct ContractLowerer<'compiler, 'program> {
    compiler: &'compiler Compiler<'program>,
    body: &'compiler Body,
    semantics: &'compiler BodySemantics,
    params: BTreeMap<ParamId, u32>,
    parameters: TypeParameters,
}

impl ContractLowerer<'_, '_> {
    fn lower_root(&self) -> LowerResult<ContractExpr> {
        self.lower_expr(self.body.root, false, &BTreeMap::new())
    }

    #[allow(clippy::too_many_lines)]
    fn lower_expr(
        &self,
        id: ExprId,
        old: bool,
        bindings: &BTreeMap<HirLocalId, u32>,
    ) -> LowerResult<ContractExpr> {
        let span = self.body.source_map.expr(id).unwrap_or_default();
        let kind = match &self.body.expressions[id] {
            loom_hir::Expr::Literal(literal) => {
                ContractExprKind::Constant(lower_literal(literal, span)?)
            }
            loom_hir::Expr::Path(_) => self.lower_resolution(id, old, bindings)?,
            loom_hir::Expr::SelfValue => ContractExprKind::Value(if old {
                ContractValue::OldSelf
            } else {
                ContractValue::SelfValue
            }),
            loom_hir::Expr::ResultValue => {
                if old {
                    return Err(defect("`old(result)` reached MIR lowering", span));
                }
                ContractExprKind::Value(ContractValue::Result)
            }
            loom_hir::Expr::Old(value) => {
                return self.lower_expr(*value, true, bindings);
            }
            loom_hir::Expr::Block { statements, tail } => {
                if !statements.is_empty() {
                    return Err(defect(
                        "contract block with statements reached MIR lowering",
                        span,
                    ));
                }
                let tail = required(*tail, "contract block has no value expression", span)?;
                return self.lower_expr(tail, old, bindings);
            }
            loom_hir::Expr::Match { scrutinee, arms } => ContractExprKind::Match {
                scrutinee: Box::new(self.lower_expr(*scrutinee, old, bindings)?),
                arms: arms
                    .iter()
                    .map(|arm| {
                        let mut arm_bindings = Vec::new();
                        let pattern = self.lower_pattern(arm.pattern, &mut arm_bindings)?;
                        let mut environment = bindings.clone();
                        let binding_base = u32::try_from(bindings.len()).map_err(|_| {
                            defect("too many outer contract pattern bindings", arm.span)
                        })?;
                        for (index, (local, _)) in arm_bindings.iter().enumerate() {
                            let index = u32::try_from(index).map_err(|_| {
                                defect("too many contract pattern bindings", arm.span)
                            })?;
                            environment.insert(
                                *local,
                                binding_base.checked_add(index).ok_or_else(|| {
                                    defect("contract binding index exceeds u32", arm.span)
                                })?,
                            );
                        }
                        Ok(ContractArm {
                            pattern,
                            bindings: arm_bindings.into_iter().map(|(_, ty)| ty).collect(),
                            value: self.lower_expr(arm.value, old, &environment)?,
                        })
                    })
                    .collect::<LowerResult<_>>()?,
            },
            loom_hir::Expr::Field { receiver, .. } => {
                let place = required(
                    self.semantics.expression_places.get(id),
                    "contract field has no resolved place",
                    span,
                )?;
                let field = place.projections.last().map(|projection| match projection {
                    PlaceProjection::Field(field) => *field,
                });
                let field = required(field, "contract field has no projection", span)?;
                ContractExprKind::Field(
                    Box::new(self.lower_expr(*receiver, old, bindings)?),
                    required(
                        self.compiler.indices.fields.get(&field).copied(),
                        "contract field has no MIR field index",
                        span,
                    )?,
                )
            }
            loom_hir::Expr::Unary { op, operand } => ContractExprKind::Unary(
                lower_unary(*op),
                Box::new(self.lower_expr(*operand, old, bindings)?),
            ),
            loom_hir::Expr::Binary { op, left, right } => {
                if matches!(op, HirBinaryOp::And | HirBinaryOp::Or) {
                    return self.lower_logical_chain(id, *op, *left, *right, old, bindings);
                }
                ContractExprKind::Binary(
                    lower_binary(*op),
                    Box::new(self.lower_expr(*left, old, bindings)?),
                    Box::new(self.lower_expr(*right, old, bindings)?),
                )
            }
            loom_hir::Expr::Call { arguments, .. } => {
                let resolution = self.call(id, span)?;
                if !matches!(
                    resolution.target,
                    SemaCallTarget::Function(definition)
                        if Some(definition) == self.compiler.analysis.canonical_std_items.is_finite
                ) || arguments.len() != 1
                {
                    return Err(defect(
                        "non-predicate call reached contract MIR lowering",
                        span,
                    ));
                }
                ContractExprKind::IsFinite(Box::new(self.lower_expr(
                    arguments[0],
                    old,
                    bindings,
                )?))
            }
            loom_hir::Expr::Error
            | loom_hir::Expr::Tuple(_)
            | loom_hir::Expr::List(_)
            | loom_hir::Expr::If { .. }
            | loom_hir::Expr::MethodCall { .. }
            | loom_hir::Expr::QualifiedMethodCall { .. }
            | loom_hir::Expr::Assign { .. }
            | loom_hir::Expr::RecordLiteral { .. }
            | loom_hir::Expr::Await(_)
            | loom_hir::Expr::Propagate(_)
            | loom_hir::Expr::Return(_) => {
                return Err(defect(
                    "non-contract expression reached contract MIR lowering",
                    span,
                ));
            }
        };
        Ok(ContractExpr { kind, span })
    }

    fn lower_logical_chain(
        &self,
        id: ExprId,
        operator: HirBinaryOp,
        left: ExprId,
        right: ExprId,
        old: bool,
        bindings: &BTreeMap<HirLocalId, u32>,
    ) -> LowerResult<ContractExpr> {
        let root_span = self.body.source_map.expr(id).unwrap_or_default();
        let mut pending = vec![right, left];
        let mut operands = Vec::new();

        while let Some(current) = pending.pop() {
            match &self.body.expressions[current] {
                loom_hir::Expr::Binary { op, left, right } if *op == operator => {
                    pending.push(*right);
                    pending.push(*left);
                }
                _ => operands.push(self.lower_expr(current, old, bindings)?),
            }
        }

        let mut expression = build_balanced_logical_contract_expression(
            lower_binary(operator),
            operands,
            root_span,
        )?;
        expression.span = root_span;
        Ok(expression)
    }

    fn lower_resolution(
        &self,
        id: ExprId,
        old: bool,
        bindings: &BTreeMap<HirLocalId, u32>,
    ) -> LowerResult<ContractExprKind> {
        let span = self.body.source_map.expr(id).unwrap_or_default();
        match required(
            self.semantics.expression_resolutions.get(id).copied(),
            "contract path has no semantic resolution",
            span,
        )? {
            Resolution::Param(parameter) => Ok(ContractExprKind::Value(if old {
                ContractValue::OldArgument(self.param_index(parameter, span)?)
            } else {
                ContractValue::Argument(self.param_index(parameter, span)?)
            })),
            Resolution::SelfValue => Ok(ContractExprKind::Value(if old {
                ContractValue::OldSelf
            } else {
                ContractValue::SelfValue
            })),
            Resolution::ResultValue if !old => Ok(ContractExprKind::Value(ContractValue::Result)),
            Resolution::Builtin(BuiltinValue::Unit) => {
                Ok(ContractExprKind::Constant(Constant::Unit))
            }
            Resolution::Definition(definition) => self
                .compiler
                .analysis
                .typed
                .constants
                .get(definition)
                .map(lower_constant_value)
                .map(ContractExprKind::Constant)
                .ok_or_else(|| {
                    defect(
                        "contract value definition has no compile-time constant",
                        span,
                    )
                }),
            Resolution::Local(local) => Ok(ContractExprKind::Binding(required(
                bindings.get(&local).copied(),
                "contract-local reference is outside its pattern arm",
                span,
            )?)),
            _ => Err(defect(
                "unsupported semantic resolution reached contract lowering",
                span,
            )),
        }
    }

    fn param_index(&self, parameter: ParamId, span: Span) -> LowerResult<u32> {
        required(
            self.params.get(&parameter).copied(),
            "contract parameter is not present in its owner signature",
            span,
        )
    }

    fn call(&self, id: ExprId, span: Span) -> LowerResult<&CallResolution> {
        required(
            self.semantics.calls.get(id),
            "contract call has no semantic call resolution",
            span,
        )
    }

    fn lower_pattern(
        &self,
        id: PatternId,
        bindings: &mut Vec<(HirLocalId, Type)>,
    ) -> LowerResult<Pattern> {
        let span = self.body.source_map.pattern(id).unwrap_or_default();
        match &self.body.patterns[id] {
            loom_hir::Pattern::Error => {
                Err(defect("error contract pattern reached MIR lowering", span))
            }
            loom_hir::Pattern::Wildcard => Ok(Pattern::Wildcard),
            loom_hir::Pattern::Literal(literal) => {
                Ok(Pattern::Constant(lower_literal(literal, span)?))
            }
            loom_hir::Pattern::Binding(local) => {
                bindings.push((*local, self.pattern_ty(id)?));
                Ok(Pattern::Binding)
            }
            loom_hir::Pattern::Name { payload, .. }
            | loom_hir::Pattern::Variant { payload, .. } => {
                match required(
                    self.semantics.pattern_resolutions.get(id).copied(),
                    "contract pattern has no semantic resolution",
                    span,
                )? {
                    Resolution::Local(local) => {
                        if !payload.is_empty() {
                            return Err(defect(
                                "contract binding pattern unexpectedly has payload",
                                span,
                            ));
                        }
                        bindings.push((local, self.pattern_ty(id)?));
                        Ok(Pattern::Binding)
                    }
                    Resolution::Definition(variant) => {
                        let (ty, variant) = required(
                            self.compiler.indices.variants.get(&variant).copied(),
                            "contract user variant has no MIR id",
                            span,
                        )?;
                        Ok(Pattern::Variant {
                            ty,
                            variant,
                            payload: payload
                                .iter()
                                .map(|child| self.lower_pattern(*child, bindings))
                                .collect::<LowerResult<_>>()?,
                        })
                    }
                    Resolution::Builtin(builtin) => {
                        let (ty, variant) = builtin_variant_id(builtin).ok_or_else(|| {
                            defect("non-variant builtin resolved as contract pattern", span)
                        })?;
                        Ok(Pattern::Variant {
                            ty,
                            variant,
                            payload: payload
                                .iter()
                                .map(|child| self.lower_pattern(*child, bindings))
                                .collect::<LowerResult<_>>()?,
                        })
                    }
                    _ => Err(defect(
                        "unsupported contract pattern resolution reached MIR lowering",
                        span,
                    )),
                }
            }
        }
    }

    fn pattern_ty(&self, id: PatternId) -> LowerResult<Type> {
        let span = self.body.source_map.pattern(id).unwrap_or_default();
        let ty = required(
            self.semantics.pattern_types.get(id).copied(),
            "contract pattern has no semantic type",
            span,
        )?;
        self.compiler.lower_ty(ty, &self.parameters, span)
    }
}

struct FunctionLowerer<'compiler, 'program> {
    compiler: &'compiler Compiler<'program>,
    body: &'compiler Body,
    semantics: &'compiler BodySemantics,
    parameters: TypeParameters,
    params: Vec<LocalDecl>,
    locals: Vec<LocalDecl>,
    param_locals: BTreeMap<ParamId, LocalId>,
    hir_locals: BTreeMap<HirLocalId, LocalId>,
    self_local: Option<LocalId>,
    next_suspend_state: u32,
    suspension_spans: Vec<(u32, Span)>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ReadMode {
    Value,
    MethodReceiver,
}

#[derive(Clone, Copy)]
enum GenericArgumentScope {
    All,
    CallOnly,
}

impl<'compiler, 'program> FunctionLowerer<'compiler, 'program> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        compiler: &'compiler Compiler<'program>,
        definition: DefId,
        body: &'compiler Body,
        semantics: &'compiler BodySemantics,
        parameters: TypeParameters,
        signature: &loom_sema::CallableSignature,
        receiver_ty: Option<TyId>,
    ) -> LowerResult<Self> {
        let span = definition_span(compiler.hir, definition);
        let mut params = Vec::new();
        let mut param_locals = BTreeMap::new();
        let self_local = if let Some(receiver_ty) = receiver_ty {
            let id = LocalId(0);
            params.push(LocalDecl {
                id,
                name: "self".into(),
                ty: compiler.lower_ty(receiver_ty, &parameters, span)?,
                mutable: signature.receiver == Some(ReceiverKind::Mutable),
                span,
            });
            Some(id)
        } else {
            None
        };
        for (parameter, ty) in &signature.params {
            let source = &compiler.hir.params[*parameter];
            let local = LocalId(u32::try_from(params.len()).map_err(|_| {
                defect(
                    "callable has more than u32::MAX parameters",
                    compiler
                        .hir
                        .source_map
                        .param(*parameter)
                        .unwrap_or_default(),
                )
            })?);
            let ty = compiler.lower_ty(
                *ty,
                &parameters,
                compiler
                    .hir
                    .source_map
                    .param(*parameter)
                    .unwrap_or_default(),
            )?;
            params.push(LocalDecl {
                id: local,
                name: source.name.to_string(),
                ty,
                mutable: false,
                span: compiler
                    .hir
                    .source_map
                    .param(*parameter)
                    .unwrap_or_default(),
            });
            param_locals.insert(*parameter, local);
        }

        let mut locals = Vec::new();
        let mut hir_locals = BTreeMap::new();
        for (local, source) in body.locals.iter() {
            let Some(ty) = semantics.local_types.get(local).copied() else {
                // HIR deliberately preallocates a potential binding for a bare
                // name-pattern. A winning nullary variant leaves that recovery
                // local untyped and therefore absent from executable MIR.
                continue;
            };
            let id = LocalId(u32::try_from(params.len() + locals.len()).map_err(|_| {
                defect(
                    "callable has more than u32::MAX locals",
                    body.source_map.local(local).unwrap_or_default(),
                )
            })?);
            let ty = compiler.lower_ty(
                ty,
                &parameters,
                body.source_map.local(local).unwrap_or_default(),
            )?;
            locals.push(LocalDecl {
                id,
                name: source.name.to_string(),
                ty,
                mutable: source.mutable || semantics.scoped_disposals.get(local).is_some(),
                span: body.source_map.local(local).unwrap_or_default(),
            });
            hir_locals.insert(local, id);
        }
        Ok(Self {
            compiler,
            body,
            semantics,
            parameters,
            params,
            locals,
            param_locals,
            hir_locals,
            self_local,
            next_suspend_state: 1,
            suspension_spans: Vec::new(),
        })
    }

    fn finish_locals(
        &self,
        body: &Block,
        receiver: Option<Receiver>,
        call_plan: &CallPlan,
    ) -> LowerResult<(Vec<LocalDecl>, Vec<LocalDecl>, Vec<SuspensionPoint>)> {
        let mut liveness = loom_mir::analyze_suspension_liveness_with_exit_contracts(
            body,
            &self.params,
            receiver,
            call_plan,
        );
        let suspension_points = self
            .suspension_spans
            .iter()
            .map(|(state, span)| {
                let live_locals = liveness.remove(state).ok_or_else(|| {
                    defect(
                        format!("lowered await state #{state} is absent from MIR liveness"),
                        *span,
                    )
                })?;
                Ok(SuspensionPoint {
                    state: *state,
                    span: *span,
                    live_locals,
                })
            })
            .collect::<LowerResult<Vec<_>>>()?;
        if let Some((state, _)) = liveness.into_iter().next() {
            return Err(defect(
                format!("MIR liveness found unknown await state #{state}"),
                body.span,
            ));
        }
        Ok((self.params.clone(), self.locals.clone(), suspension_points))
    }

    fn lower_root(&mut self) -> LowerResult<Block> {
        self.lower_as_block(self.body.root)
    }

    fn lower_as_block(&mut self, id: ExprId) -> LowerResult<Block> {
        let span = self.expr_span(id);
        let loom_hir::Expr::Block { statements, tail } = self.body.expressions[id].clone() else {
            let mut statements = Vec::new();
            let tail = self.lower_suspendable_expr(id, &mut statements, true)?;
            return Ok(Block {
                statements,
                tail: Some(Box::new(tail)),
                span,
            });
        };
        let mut lowered = Vec::with_capacity(statements.len());
        for statement in statements {
            self.lower_statement(&statement, &mut lowered)?;
        }
        let tail = if let Some(tail) = tail {
            match self.body.expressions[tail].clone() {
                loom_hir::Expr::Return(value) => {
                    let value = if let Some(value) = value {
                        Some(self.lower_suspendable_expr(value, &mut lowered, true)?)
                    } else {
                        None
                    };
                    lowered.push(Statement {
                        kind: StatementKind::Return(value),
                        span: self.expr_span(tail),
                    });
                    None
                }
                loom_hir::Expr::Assign { target, value } => {
                    let value = self.lower_suspendable_expr(value, &mut lowered, false)?;
                    lowered.push(Statement {
                        kind: StatementKind::Assign {
                            place: self.expression_place(target)?,
                            value,
                        },
                        span: self.expr_span(tail),
                    });
                    None
                }
                _ => Some(Box::new(self.lower_suspendable_expr(
                    tail,
                    &mut lowered,
                    true,
                )?)),
            }
        } else {
            None
        };
        Ok(Block {
            statements: lowered,
            tail,
            span,
        })
    }

    #[allow(clippy::too_many_lines)]
    fn lower_statement(
        &mut self,
        source: &HirStatement,
        output: &mut Vec<Statement>,
    ) -> LowerResult<()> {
        match source {
            HirStatement::Let { local, value } => {
                let mir_local = required(
                    self.hir_locals.get(local).copied(),
                    "binding has no MIR local",
                    self.expr_span(*value),
                )?;
                let lowered_value = self.lower_suspendable_expr(*value, output, true)?;
                output.push(Statement {
                    kind: StatementKind::Let {
                        local: mir_local,
                        value: lowered_value,
                    },
                    span: self.expr_span(*value),
                });
            }
            HirStatement::Scoped { local, value } => {
                let span = self.expr_span(*value);
                let mir_local = required(
                    self.hir_locals.get(local).copied(),
                    "scoped binding has no MIR local",
                    span,
                )?;
                let lowered_value = self.lower_suspendable_expr(*value, output, true)?;
                let disposal = required(
                    self.semantics.scoped_disposals.get(*local),
                    "scoped binding has no selected Dispose witness",
                    span,
                )?;
                let disposal = match disposal {
                    SemaScopedDisposal::Concept {
                        requirement,
                        witness,
                    } => {
                        let requirement = required(
                            self.compiler.indices.requirements.get(requirement).copied(),
                            "Dispose requirement has no MIR id",
                            span,
                        )?;
                        let dispatch_type = self
                            .params
                            .iter()
                            .chain(&self.locals)
                            .find(|declaration| declaration.id == mir_local)
                            .map(|declaration| declaration.ty.clone())
                            .ok_or_else(|| defect("scoped MIR local has no declaration", span))?;
                        MirScopedDisposal::StaticConcept {
                            requirement,
                            witness: self.lower_witness_selection(witness, span)?,
                            dispatch_type,
                        }
                    }
                };
                output.push(Statement {
                    kind: StatementKind::Scoped {
                        local: mir_local,
                        value: lowered_value,
                        disposal,
                    },
                    span,
                });
            }
            HirStatement::LetTuple { locals, value } => {
                let locals = locals
                    .iter()
                    .map(|local| {
                        required(
                            self.hir_locals.get(local).copied(),
                            "tuple binding has no MIR local",
                            self.expr_span(*value),
                        )
                    })
                    .collect::<LowerResult<Vec<_>>>()?;
                let lowered_value = self.lower_suspendable_expr(*value, output, true)?;
                output.push(Statement {
                    kind: StatementKind::LetTuple {
                        locals,
                        value: lowered_value,
                    },
                    span: self.expr_span(*value),
                });
            }
            HirStatement::ForRange {
                local,
                start,
                end,
                body,
            } => {
                let local = required(
                    self.hir_locals.get(local).copied(),
                    "range binding has no MIR local",
                    self.expr_span(*body),
                )?;
                let start = self.lower_suspendable_expr(*start, output, false)?;
                let end = self.lower_suspendable_expr(*end, output, false)?;
                output.push(Statement {
                    kind: StatementKind::ForRange {
                        local,
                        start: Box::new(start),
                        end: Box::new(end),
                        body: Box::new(self.lower_as_block(*body)?),
                    },
                    span: self.expr_span(*body),
                });
            }
            HirStatement::While { condition, body } => {
                let condition = self.lower_expr(*condition)?;
                output.push(Statement {
                    kind: StatementKind::While {
                        condition: Box::new(condition),
                        body: Box::new(self.lower_as_block(*body)?),
                    },
                    span: self.expr_span(*body),
                });
            }
            HirStatement::Break { span } => output.push(Statement {
                kind: StatementKind::Break,
                span: *span,
            }),
            HirStatement::Continue { span } => output.push(Statement {
                kind: StatementKind::Continue,
                span: *span,
            }),
            HirStatement::Defer { body } => output.push(Statement {
                kind: StatementKind::Defer(self.lower_as_block(*body)?),
                span: self.expr_span(*body),
            }),
            HirStatement::Assert(condition) => {
                let check = required(
                    self.semantics.assertion_checks.get(*condition).copied(),
                    "assertion has no proof disposition",
                    self.expr_span(*condition),
                )?;
                let restores_receiver = self
                    .semantics
                    .receiver_invariant_recoveries
                    .contains(condition);
                if check != RuntimeCheck::Proven {
                    let condition = self.lower_suspendable_expr(*condition, output, false)?;
                    output.push(Statement {
                        span: condition.span,
                        kind: StatementKind::Assert { condition },
                    });
                }
                if restores_receiver {
                    output.push(Statement {
                        span: self.expr_span(*condition),
                        kind: StatementKind::RestoreReceiverInvariant {
                            check: ReceiverInvariantCheck::Proven,
                        },
                    });
                }
            }
            HirStatement::Discard(expression) => {
                let expression = self.lower_suspendable_expr(*expression, output, true)?;
                output.push(Statement {
                    span: expression.span,
                    kind: StatementKind::Evaluate(expression),
                });
            }
            HirStatement::Expr(expression) => {
                let span = self.expr_span(*expression);
                let kind = match self.body.expressions[*expression].clone() {
                    loom_hir::Expr::Assign { target, value } => StatementKind::Assign {
                        place: self.expression_place(target)?,
                        value: self.lower_suspendable_expr(value, output, false)?,
                    },
                    loom_hir::Expr::Return(value) => {
                        let value = if let Some(value) = value {
                            Some(self.lower_suspendable_expr(value, output, true)?)
                        } else {
                            None
                        };
                        StatementKind::Return(value)
                    }
                    _ => StatementKind::Evaluate(self.lower_suspendable_expr(
                        *expression,
                        output,
                        true,
                    )?),
                };
                output.push(Statement { kind, span });
            }
        }
        Ok(())
    }

    fn lower_expr(&mut self, id: ExprId) -> LowerResult<Expr> {
        self.lower_expr_mode(id, ReadMode::Value)
    }

    fn lower_suspendable_expr(
        &mut self,
        id: ExprId,
        output: &mut Vec<Statement>,
        allow_root_await: bool,
    ) -> LowerResult<Expr> {
        let expression = self.lower_expr(id)?;
        self.extract_nested_awaits(expression, output, allow_root_await)
    }

    #[allow(clippy::too_many_lines)]
    fn extract_nested_awaits(
        &mut self,
        expression: Expr,
        output: &mut Vec<Statement>,
        allow_root_await: bool,
    ) -> LowerResult<Expr> {
        let Expr { kind, ty, span, .. } = expression;
        let kind = match kind {
            ExprKind::Await { state, task } => {
                let task = self.extract_nested_awaits(*task, output, false)?;
                let awaited = Expr {
                    id: loom_mir::ExprId::UNASSIGNED,
                    kind: ExprKind::Await {
                        state,
                        task: Box::new(task),
                    },
                    ty: ty.clone(),
                    span,
                };
                if allow_root_await {
                    return Ok(awaited);
                }
                let local = self.add_temp(format!("$await{state}"), ty.clone(), span)?;
                output.push(Statement {
                    kind: StatementKind::Let {
                        local,
                        value: awaited,
                    },
                    span,
                });
                return Ok(Expr {
                    id: loom_mir::ExprId::UNASSIGNED,
                    kind: ExprKind::Move(loom_mir::Place::local(local)),
                    ty,
                    span,
                });
            }
            ExprKind::Tuple(elements) => ExprKind::Tuple(
                elements
                    .into_iter()
                    .map(|element| self.extract_nested_awaits(element, output, false))
                    .collect::<LowerResult<_>>()?,
            ),
            ExprKind::List(elements) => ExprKind::List(
                elements
                    .into_iter()
                    .map(|element| self.extract_nested_awaits(element, output, false))
                    .collect::<LowerResult<_>>()?,
            ),
            ExprKind::Unary(operator, value) => ExprKind::Unary(
                operator,
                Box::new(self.extract_nested_awaits(*value, output, false)?),
            ),
            ExprKind::Binary(operator, left, right) => ExprKind::Binary(
                operator,
                Box::new(self.extract_nested_awaits(*left, output, false)?),
                Box::new(self.extract_nested_awaits(*right, output, false)?),
            ),
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => ExprKind::If {
                condition: Box::new(self.extract_nested_awaits(*condition, output, false)?),
                then_branch,
                else_branch,
            },
            ExprKind::Match { scrutinee, arms } => ExprKind::Match {
                scrutinee: Box::new(self.extract_nested_awaits(*scrutinee, output, false)?),
                arms,
            },
            ExprKind::Record {
                ty,
                type_arguments,
                fields,
                construction,
            } => ExprKind::Record {
                ty,
                type_arguments,
                fields: fields
                    .into_iter()
                    .map(|field| self.extract_nested_awaits(field, output, false))
                    .collect::<LowerResult<_>>()?,
                construction,
            },
            ExprKind::Variant {
                ty,
                type_arguments,
                variant,
                payload,
            } => ExprKind::Variant {
                ty,
                type_arguments,
                variant,
                payload: payload
                    .into_iter()
                    .map(|value| self.extract_nested_awaits(value, output, false))
                    .collect::<LowerResult<_>>()?,
            },
            ExprKind::Refine {
                ty,
                value,
                construction,
            } => ExprKind::Refine {
                ty,
                value: Box::new(self.extract_nested_awaits(*value, output, false)?),
                construction,
            },
            ExprKind::Unrefine(value) => {
                ExprKind::Unrefine(Box::new(self.extract_nested_awaits(*value, output, false)?))
            }
            ExprKind::Sleep { milliseconds } => ExprKind::Sleep {
                milliseconds: Box::new(self.extract_nested_awaits(*milliseconds, output, false)?),
            },
            ExprKind::TaskJoin { mode, arguments } => ExprKind::TaskJoin {
                mode,
                arguments: arguments
                    .into_iter()
                    .map(|argument| self.extract_nested_awaits(argument, output, false))
                    .collect::<LowerResult<_>>()?,
            },
            ExprKind::Call {
                target,
                type_arguments,
                arguments,
                witnesses,
            } => ExprKind::Call {
                target,
                type_arguments,
                arguments: arguments
                    .into_iter()
                    .map(|argument| match argument {
                        CallArgument::Value(value) => self
                            .extract_nested_awaits(value, output, false)
                            .map(CallArgument::Value),
                        CallArgument::InOut(place) => Ok(CallArgument::InOut(place)),
                    })
                    .collect::<LowerResult<_>>()?,
                witnesses,
            },
            ExprKind::MakeView {
                value,
                writeback,
                witness,
                mutable,
                token,
            } => ExprKind::MakeView {
                value: Box::new(self.extract_nested_awaits(*value, output, false)?),
                writeback,
                witness,
                mutable,
                token,
            },
            ExprKind::Constant(_)
            | ExprKind::Copy(_)
            | ExprKind::Move(_)
            | ExprKind::Block(_)
            | ExprKind::ReborrowView { .. } => kind,
        };
        Ok(Expr::new(kind, ty, span))
    }

    fn lower_expr_mode(&mut self, id: ExprId, mode: ReadMode) -> LowerResult<Expr> {
        let span = self.expr_span(id);
        if matches!(
            self.semantics.expression_coercions.get(id),
            Some(Coercion::ConcreteToDyn | Coercion::InterfaceReborrow)
        ) {
            let view = required(
                self.semantics.views.get(id),
                "dynamic coercion has no selected witness",
                span,
            )?;
            let kind = match &view.source {
                ViewSource::Concrete { witness, writeback } => {
                    let mut value = self.lower_expr_uncoerced(id, mode)?;
                    value.ty = self.lower_witness_target(witness, span)?;
                    ExprKind::MakeView {
                        value: Box::new(value),
                        writeback: writeback
                            .as_ref()
                            .map(|owner| self.lower_place(owner, span))
                            .transpose()?,
                        witness: self.lower_witness_selection(witness, span)?,
                        mutable: view.mutable,
                        token: view.token.0,
                    }
                }
                ViewSource::Interface { owner } => ExprKind::ReborrowView {
                    owner: self.lower_place(owner, span)?,
                    mutable: view.mutable,
                    token: view.token.0,
                },
            };
            return Ok(Expr {
                id: loom_mir::ExprId::UNASSIGNED,
                kind,
                ty: self.expression_ty(id)?,
                span,
            });
        }
        if let Some(Coercion::RefinedToBase { refined }) =
            self.semantics.expression_coercions.get(id).copied()
        {
            let final_ty = self.expression_ty(id)?;
            let mut value = self.lower_expr_uncoerced(id, mode)?;
            value.ty = Type::Nominal(
                required(
                    self.compiler.indices.types.get(&refined).copied(),
                    "refinement coercion target has no MIR type id",
                    span,
                )?,
                Vec::new(),
            );
            return Ok(Expr {
                id: loom_mir::ExprId::UNASSIGNED,
                kind: ExprKind::Unrefine(Box::new(value)),
                ty: final_ty,
                span,
            });
        }
        self.lower_expr_uncoerced(id, mode)
    }

    #[allow(clippy::too_many_lines)]
    fn lower_expr_uncoerced(&mut self, id: ExprId, mode: ReadMode) -> LowerResult<Expr> {
        let span = self.expr_span(id);
        let ty = self.uncoerced_expression_ty(id)?;
        let source = self.body.expressions[id].clone();
        let kind = match source {
            loom_hir::Expr::Error => return Err(defect("error expression reached MIR", span)),
            loom_hir::Expr::Literal(literal) => {
                if let Literal::Int(value) = &literal
                    && value == "9223372036854775808"
                {
                    return Err(defect(
                        "Int.MIN magnitude reached MIR outside direct unary negation",
                        span,
                    ));
                }
                ExprKind::Constant(lower_literal(&literal, span)?)
            }
            loom_hir::Expr::Tuple(elements) => ExprKind::Tuple(
                elements
                    .into_iter()
                    .map(|element| self.lower_expr(element))
                    .collect::<LowerResult<_>>()?,
            ),
            loom_hir::Expr::List(elements) => ExprKind::List(
                elements
                    .into_iter()
                    .map(|element| self.lower_expr(element))
                    .collect::<LowerResult<_>>()?,
            ),
            loom_hir::Expr::Path(_) | loom_hir::Expr::SelfValue => {
                self.lower_value_path(id, mode)?
            }
            loom_hir::Expr::ResultValue | loom_hir::Expr::Old(_) => {
                return Err(defect(
                    "contract-only value reached executable body MIR lowering",
                    span,
                ));
            }
            loom_hir::Expr::Block { .. } => ExprKind::Block(self.lower_as_block(id)?),
            loom_hir::Expr::If {
                condition,
                then_branch,
                else_branch,
            } => ExprKind::If {
                condition: Box::new(self.lower_expr(condition)?),
                then_branch: self.lower_as_block(then_branch)?,
                else_branch: else_branch.map_or_else(
                    || {
                        Ok(Block {
                            statements: Vec::new(),
                            tail: None,
                            span,
                        })
                    },
                    |branch| self.lower_as_block(branch),
                )?,
            },
            loom_hir::Expr::Match { scrutinee, arms } => ExprKind::Match {
                scrutinee: Box::new(self.lower_expr(scrutinee)?),
                arms: arms
                    .iter()
                    .map(|arm| {
                        let mut bindings = Vec::new();
                        let pattern = lower_pattern(
                            self.compiler,
                            self.body,
                            self.semantics,
                            arm.pattern,
                            &self.hir_locals,
                            &mut bindings,
                        )?;
                        Ok(MatchArm {
                            pattern,
                            bindings,
                            value: self.lower_expr(arm.value)?,
                        })
                    })
                    .collect::<LowerResult<_>>()?,
            },
            loom_hir::Expr::Call { arguments, .. } => {
                return self.lower_resolved_call(id, None, &arguments);
            }
            loom_hir::Expr::MethodCall {
                receiver,
                arguments,
                ..
            } => return self.lower_resolved_call(id, Some(receiver), &arguments),
            loom_hir::Expr::QualifiedMethodCall { arguments, .. } => {
                let resolution = self.call(id)?;
                let (receiver, arguments) = if resolution.receiver.is_some() {
                    let (receiver, arguments) = arguments.split_first().ok_or_else(|| {
                        defect("qualified receiver call has no receiver argument", span)
                    })?;
                    (Some(*receiver), arguments)
                } else {
                    (None, arguments.as_slice())
                };
                return self.lower_resolved_call(id, receiver, arguments);
            }
            loom_hir::Expr::Field { .. } => {
                if self.semantics.calls.get(id).is_some() {
                    return self.lower_resolved_call(id, None, &[]);
                }
                self.lower_place_read(id, mode)?
            }
            loom_hir::Expr::Unary { op, operand } => {
                if op == HirUnaryOp::Negate {
                    if let loom_hir::Expr::Literal(Literal::Int(value)) =
                        &self.body.expressions[operand]
                    {
                        if value == "9223372036854775808" {
                            ExprKind::Constant(Constant::Int(i64::MIN))
                        } else {
                            ExprKind::Unary(
                                lower_unary(op),
                                Box::new(self.lower_numeric_operand(operand)?),
                            )
                        }
                    } else {
                        ExprKind::Unary(
                            lower_unary(op),
                            Box::new(self.lower_numeric_operand(operand)?),
                        )
                    }
                } else {
                    ExprKind::Unary(lower_unary(op), Box::new(self.lower_expr(operand)?))
                }
            }
            loom_hir::Expr::Binary { op, left, right } => {
                if matches!(op, HirBinaryOp::And | HirBinaryOp::Or) {
                    return self.lower_logical_chain(id, op, left, right);
                }
                let numeric_operator = matches!(
                    op,
                    HirBinaryOp::Add
                        | HirBinaryOp::Subtract
                        | HirBinaryOp::Multiply
                        | HirBinaryOp::Divide
                        | HirBinaryOp::Less
                        | HirBinaryOp::LessEqual
                        | HirBinaryOp::Greater
                        | HirBinaryOp::GreaterEqual
                );
                let left_numeric = self.numeric_base(left)?;
                let numeric_equality = matches!(op, HirBinaryOp::Equal | HirBinaryOp::NotEqual)
                    && left_numeric.is_some()
                    && left_numeric == self.numeric_base(right)?;
                let numeric = numeric_operator || numeric_equality;
                let left = if numeric {
                    self.lower_numeric_operand(left)?
                } else {
                    self.lower_expr(left)?
                };
                let right = if numeric {
                    self.lower_numeric_operand(right)?
                } else {
                    self.lower_expr(right)?
                };
                ExprKind::Binary(lower_binary(op), Box::new(left), Box::new(right))
            }
            loom_hir::Expr::Assign { .. } => {
                return Err(defect("assignment reached MIR expression position", span));
            }
            loom_hir::Expr::RecordLiteral { fields, .. } => {
                return self.lower_record_literal(id, &fields);
            }
            loom_hir::Expr::Await(value) => {
                let state = self.next_suspend_state;
                self.next_suspend_state = self
                    .next_suspend_state
                    .checked_add(1)
                    .ok_or_else(|| defect("too many async suspension points", span))?;
                self.suspension_spans.push((state, span));
                ExprKind::Await {
                    state,
                    task: Box::new(self.lower_expr(value)?),
                }
            }
            loom_hir::Expr::Propagate(value) => {
                return self.lower_propagate(value, ty, span);
            }
            loom_hir::Expr::Return(_) => {
                return Err(defect("return reached MIR expression position", span));
            }
        };
        Ok(Expr::new(kind, ty, span))
    }

    /// Lowers an associative short-circuit chain without retaining the parser's
    /// left-deep shape in MIR.
    ///
    /// The parser deliberately builds binary operators iteratively, but a long
    /// `&&` or `||` chain is still represented as a left-deep HIR tree. Walking
    /// that tree recursively consumed the process stack in MIR lowering, and
    /// every later recursive MIR visitor inherited the same depth. Logical
    /// conjunction and disjunction may be re-associated without changing their
    /// left-to-right evaluation or short-circuit behavior, so collect the
    /// operands iteratively and construct a balanced MIR tree.
    fn lower_logical_chain(
        &mut self,
        id: ExprId,
        operator: HirBinaryOp,
        left: ExprId,
        right: ExprId,
    ) -> LowerResult<Expr> {
        let root_span = self.expr_span(id);
        let root_ty = self.uncoerced_expression_ty(id)?;
        let mut pending = vec![right, left];
        let mut operands = Vec::new();

        while let Some(current) = pending.pop() {
            let source = self.body.expressions[current].clone();
            match source {
                loom_hir::Expr::Binary { op, left, right }
                    if op == operator
                        && self.semantics.expression_coercions.get(current).is_none() =>
                {
                    pending.push(right);
                    pending.push(left);
                }
                _ => operands.push(self.lower_expr(current)?),
            }
        }

        let mut expression =
            build_balanced_logical_expression(lower_binary(operator), operands, root_span)?;
        expression.ty = root_ty;
        expression.span = root_span;
        Ok(expression)
    }

    fn lower_propagate(&mut self, value: ExprId, ok_ty: Type, span: Span) -> LowerResult<Expr> {
        let operand_ty = self.expression_ty(value)?;
        let operand_arguments = Self::checked_result_arguments(
            &operand_ty,
            "checked `?` operand has an invalid Result shape",
            span,
        )?;
        let success_ty = operand_arguments[0].clone();
        let error_ty = operand_arguments[1].clone();

        let Signature::Callable(signature) = self.compiler.signature(self.body.owner)? else {
            return Err(defect("`?` owner has no callable signature", span));
        };
        let return_ty = self
            .compiler
            .lower_ty(signature.return_ty, &self.parameters, span)?;
        let return_arguments = Self::checked_result_arguments(
            &return_ty,
            "checked `?` owner has an invalid Result return shape",
            span,
        )?
        .to_vec();
        if error_ty != return_arguments[1] {
            return Err(defect(
                "checked `?` error type differs from the callable return error type",
                span,
            ));
        }
        let success_local = self.add_temp(
            format!("$propagate_ok{}", self.locals.len()),
            success_ty.clone(),
            span,
        )?;
        let error_local = self.add_temp(
            format!("$propagate_err{}", self.locals.len()),
            error_ty.clone(),
            span,
        )?;

        let success = Expr {
            id: loom_mir::ExprId::UNASSIGNED,
            kind: ExprKind::Move(loom_mir::Place::local(success_local)),
            ty: success_ty,
            span,
        };
        let propagated_error = Expr {
            id: loom_mir::ExprId::UNASSIGNED,
            kind: ExprKind::Variant {
                ty: RESULT_TYPE,
                type_arguments: return_arguments,
                variant: VariantId(1),
                payload: vec![Expr {
                    id: loom_mir::ExprId::UNASSIGNED,
                    kind: ExprKind::Move(loom_mir::Place::local(error_local)),
                    ty: error_ty,
                    span,
                }],
            },
            ty: return_ty,
            span,
        };
        let early_return = Expr {
            id: loom_mir::ExprId::UNASSIGNED,
            kind: ExprKind::Block(Block {
                statements: vec![Statement {
                    kind: StatementKind::Return(Some(propagated_error)),
                    span,
                }],
                tail: None,
                span,
            }),
            ty: Type::Never,
            span,
        };

        Ok(Expr {
            id: loom_mir::ExprId::UNASSIGNED,
            kind: ExprKind::Match {
                scrutinee: Box::new(self.lower_expr(value)?),
                arms: vec![
                    MatchArm {
                        pattern: Pattern::Variant {
                            ty: RESULT_TYPE,
                            variant: VariantId(0),
                            payload: vec![Pattern::Binding],
                        },
                        bindings: vec![success_local],
                        value: success,
                    },
                    MatchArm {
                        pattern: Pattern::Variant {
                            ty: RESULT_TYPE,
                            variant: VariantId(1),
                            payload: vec![Pattern::Binding],
                        },
                        bindings: vec![error_local],
                        value: early_return,
                    },
                ],
            },
            ty: ok_ty,
            span,
        })
    }

    fn checked_result_arguments<'ty>(
        ty: &'ty Type,
        message: &'static str,
        span: Span,
    ) -> LowerResult<&'ty [Type]> {
        match ty {
            Type::Nominal(result, arguments) if *result == RESULT_TYPE && arguments.len() == 2 => {
                Ok(arguments)
            }
            _ => Err(defect(message, span)),
        }
    }

    fn lower_numeric_operand(&mut self, id: ExprId) -> LowerResult<Expr> {
        let span = self.expr_span(id);
        let value = self.lower_expr(id)?;
        let semantic = self.expression_semantic_ty(id)?;
        let TyData::Nominal { definition, .. } = self.compiler.analysis.typed.types.data(semantic)
        else {
            return Ok(value);
        };
        let DefinitionKind::RefinedType(refined) = &self.compiler.hir.definitions[*definition].kind
        else {
            return Ok(value);
        };
        let base_semantic = self.compiler.resolved_type_ref(refined.base)?;
        let base = self
            .compiler
            .lower_ty(base_semantic, &self.parameters, span)?;
        if !matches!(base, Type::Int | Type::Float) {
            return Ok(value);
        }
        Ok(Expr {
            id: loom_mir::ExprId::UNASSIGNED,
            kind: ExprKind::Unrefine(Box::new(value)),
            ty: base,
            span,
        })
    }

    fn numeric_base(&self, id: ExprId) -> LowerResult<Option<BuiltinType>> {
        let ty = self.expression_semantic_ty(id)?;
        match self.compiler.analysis.typed.types.data(ty) {
            TyData::Builtin(BuiltinType::Int) => Ok(Some(BuiltinType::Int)),
            TyData::Builtin(BuiltinType::Float) => Ok(Some(BuiltinType::Float)),
            TyData::Nominal { definition, .. } => {
                let DefinitionKind::RefinedType(refined) =
                    &self.compiler.hir.definitions[*definition].kind
                else {
                    return Ok(None);
                };
                let base = self.compiler.resolved_type_ref(refined.base)?;
                match self.compiler.analysis.typed.types.data(base) {
                    TyData::Builtin(BuiltinType::Int) => Ok(Some(BuiltinType::Int)),
                    TyData::Builtin(BuiltinType::Float) => Ok(Some(BuiltinType::Float)),
                    _ => Ok(None),
                }
            }
            _ => Ok(None),
        }
    }

    fn lower_value_path(&self, id: ExprId, mode: ReadMode) -> LowerResult<ExprKind> {
        let span = self.expr_span(id);
        match required(
            self.semantics.expression_resolutions.get(id).copied(),
            "value path has no semantic resolution",
            span,
        )? {
            Resolution::Builtin(BuiltinValue::Unit) => Ok(ExprKind::Constant(Constant::Unit)),
            Resolution::Builtin(BuiltinValue::None) => Ok(ExprKind::Variant {
                ty: OPTION_TYPE,
                type_arguments: Self::nominal_type_arguments(
                    &self.uncoerced_expression_ty(id)?,
                    OPTION_TYPE,
                    span,
                )?,
                variant: VariantId(0),
                payload: Vec::new(),
            }),
            Resolution::Definition(definition) => self
                .compiler
                .analysis
                .typed
                .constants
                .get(definition)
                .map(lower_constant_value)
                .map(ExprKind::Constant)
                .ok_or_else(|| defect("value definition is not a constant", span)),
            Resolution::Param(_) | Resolution::Local(_) | Resolution::SelfValue => {
                self.lower_place_read(id, mode)
            }
            _ => Err(defect(
                "non-value resolution reached executable path lowering",
                span,
            )),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn lower_resolved_call(
        &mut self,
        id: ExprId,
        receiver: Option<ExprId>,
        source_arguments: &[ExprId],
    ) -> LowerResult<Expr> {
        let span = self.expr_span(id);
        let ty = self.uncoerced_expression_ty(id)?;
        let resolution = self.call(id)?.clone();

        match resolution.target {
            SemaCallTarget::EnumVariant(variant) => {
                let (enum_ty, variant) = required(
                    self.compiler.indices.variants.get(&variant).copied(),
                    "enum constructor has no MIR variant id",
                    span,
                )?;
                let type_arguments = Self::nominal_type_arguments(&ty, enum_ty, span)?;
                return Ok(Expr {
                    id: loom_mir::ExprId::UNASSIGNED,
                    kind: ExprKind::Variant {
                        ty: enum_ty,
                        type_arguments,
                        variant,
                        payload: source_arguments
                            .iter()
                            .map(|argument| self.lower_expr(*argument))
                            .collect::<LowerResult<_>>()?,
                    },
                    ty,
                    span,
                });
            }
            SemaCallTarget::RefinedConstructor(definition) => {
                let value = required(
                    source_arguments.first().copied(),
                    "refined constructor has no value argument",
                    span,
                )?;
                return Ok(Expr {
                    id: loom_mir::ExprId::UNASSIGNED,
                    kind: ExprKind::Refine {
                        ty: required(
                            self.compiler.indices.types.get(&definition).copied(),
                            "refined constructor has no MIR type id",
                            span,
                        )?,
                        value: Box::new(self.lower_expr(value)?),
                        construction: match required(
                            self.semantics.construction_checks.get(id).copied(),
                            "refined constructor has no proof disposition",
                            span,
                        )? {
                            ConstructionCheck::Proven => ConstructionMode::Proven,
                            ConstructionCheck::Runtime => ConstructionMode::Runtime,
                        },
                    },
                    ty,
                    span,
                });
            }
            SemaCallTarget::Builtin(builtin) => {
                let (receiver, arguments) = match (resolution.receiver, receiver) {
                    (Some(_), Some(receiver)) => (Some(receiver), source_arguments),
                    (Some(_), None) => {
                        let Some((receiver, arguments)) = source_arguments.split_first() else {
                            return Err(defect(
                                "authenticated builtin receiver call has no source receiver",
                                span,
                            ));
                        };
                        (Some(*receiver), arguments)
                    }
                    // Static builtin members retain a syntactic qualifier in
                    // HIR, but semantic analysis intentionally grants them no
                    // runtime receiver.
                    (None, None | Some(_)) => (None, source_arguments),
                };
                return self.lower_builtin_call(id, builtin, receiver, arguments);
            }
            SemaCallTarget::TaskIntrinsic(intrinsic) => {
                if resolution.receiver.is_some() {
                    return Err(defect(
                        "Task intrinsic unexpectedly carries a receiver",
                        span,
                    ));
                }
                return self.lower_task_intrinsic(id, intrinsic, source_arguments);
            }
            _ => {}
        }

        let mut arguments =
            Vec::with_capacity(source_arguments.len() + usize::from(receiver.is_some()));
        if let Some(receiver) = receiver {
            match resolution.receiver {
                Some(loom_sema::ReceiverPassing::InOut) => {
                    arguments.push(CallArgument::InOut(self.expression_place(receiver)?));
                }
                Some(loom_sema::ReceiverPassing::Value) => {
                    arguments.push(CallArgument::Value(
                        self.lower_expr_mode(receiver, ReadMode::MethodReceiver)?,
                    ));
                }
                None => {
                    return Err(defect(
                        "source receiver is absent from semantic call resolution",
                        span,
                    ));
                }
            }
        } else if resolution.receiver.is_some() {
            return Err(defect(
                "semantic receiver call has no source receiver",
                span,
            ));
        }
        arguments.extend(
            source_arguments
                .iter()
                .map(|argument| self.lower_expr(*argument).map(CallArgument::Value))
                .collect::<LowerResult<Vec<_>>>()?,
        );

        let witnesses = resolution
            .witnesses
            .iter()
            .map(|selection| self.lower_witness_selection(selection, span))
            .collect::<LowerResult<_>>()?;
        let (target, type_arguments) = match resolution.target {
            SemaCallTarget::Function(definition) => (
                CallTarget::Direct(required(
                    self.compiler.indices.functions.get(&definition).copied(),
                    "direct call target has no MIR function id",
                    span,
                )?),
                self.lower_call_type_arguments(
                    definition,
                    GenericArgumentScope::All,
                    &resolution.substitution,
                    span,
                )?,
            ),
            SemaCallTarget::InherentMethod(definition) => {
                let function = required(
                    self.compiler.indices.functions.get(&definition).copied(),
                    "inherent call target has no MIR function id",
                    span,
                )?;
                let Signature::Callable(signature) = self.compiler.signature(definition)? else {
                    return Err(defect(
                        "inherent call target has no callable signature",
                        span,
                    ));
                };
                let target = if matches!(
                    signature.receiver,
                    Some(ReceiverKind::ReadOnly | ReceiverKind::Mutable)
                ) {
                    CallTarget::Inherent(function)
                } else {
                    CallTarget::Direct(function)
                };
                (
                    target,
                    self.lower_call_type_arguments(
                        definition,
                        GenericArgumentScope::All,
                        &resolution.substitution,
                        span,
                    )?,
                )
            }
            SemaCallTarget::StaticConcept { requirement } => (
                CallTarget::StaticConcept {
                    requirement: required(
                        self.compiler
                            .indices
                            .requirements
                            .get(&requirement)
                            .copied(),
                        "static concept call requirement has no MIR id",
                        span,
                    )?,
                    witness: self.lower_witness_selection(
                        required(
                            resolution.dispatch_witness.as_ref(),
                            "static concept call has no dispatch witness",
                            span,
                        )?,
                        span,
                    )?,
                    dispatch_type: self.lower_witness_target(
                        required(
                            resolution.dispatch_witness.as_ref(),
                            "static concept call has no dispatch witness",
                            span,
                        )?,
                        span,
                    )?,
                },
                self.lower_call_type_arguments(
                    requirement,
                    GenericArgumentScope::CallOnly,
                    &resolution.substitution,
                    span,
                )?,
            ),
            SemaCallTarget::DynamicConcept { requirement } => (
                CallTarget::Dynamic {
                    requirement: required(
                        self.compiler
                            .indices
                            .requirements
                            .get(&requirement)
                            .copied(),
                        "dynamic concept call requirement has no MIR id",
                        span,
                    )?,
                },
                Vec::new(),
            ),
            SemaCallTarget::EnumVariant(_)
            | SemaCallTarget::RefinedConstructor(_)
            | SemaCallTarget::Builtin(_)
            | SemaCallTarget::TaskIntrinsic(_) => unreachable!("handled above"),
            SemaCallTarget::Error => {
                return Err(defect("error call target reached MIR lowering", span));
            }
        };
        Ok(Expr {
            id: loom_mir::ExprId::UNASSIGNED,
            kind: ExprKind::Call {
                target,
                type_arguments,
                arguments,
                witnesses,
            },
            ty,
            span,
        })
    }

    fn lower_task_intrinsic(
        &mut self,
        id: ExprId,
        intrinsic: TaskIntrinsic,
        arguments: &[ExprId],
    ) -> LowerResult<Expr> {
        let span = self.expr_span(id);
        let ty = self.uncoerced_expression_ty(id)?;
        let kind = match intrinsic {
            TaskIntrinsic::Sleep => {
                let [milliseconds] = arguments else {
                    return Err(defect("checked Task.sleep call has invalid arity", span));
                };
                ExprKind::Sleep {
                    milliseconds: Box::new(self.lower_expr(*milliseconds)?),
                }
            }
            TaskIntrinsic::All
            | TaskIntrinsic::Settled
            | TaskIntrinsic::Any
            | TaskIntrinsic::Race => ExprKind::TaskJoin {
                mode: match intrinsic {
                    TaskIntrinsic::All => loom_mir::TaskJoinMode::All,
                    TaskIntrinsic::Settled => loom_mir::TaskJoinMode::Settled,
                    TaskIntrinsic::Any => loom_mir::TaskJoinMode::Any,
                    TaskIntrinsic::Race => loom_mir::TaskJoinMode::Race,
                    TaskIntrinsic::Sleep => unreachable!(),
                },
                arguments: arguments
                    .iter()
                    .map(|argument| self.lower_expr(*argument))
                    .collect::<LowerResult<_>>()?,
            },
        };
        Ok(Expr::new(kind, ty, span))
    }

    #[allow(clippy::too_many_lines)]
    fn lower_builtin_call(
        &mut self,
        id: ExprId,
        builtin: BuiltinValue,
        receiver: Option<ExprId>,
        arguments: &[ExprId],
    ) -> LowerResult<Expr> {
        let span = self.expr_span(id);
        let ty = self.uncoerced_expression_ty(id)?;
        let kind = match builtin {
            BuiltinValue::ListNew => ExprKind::List(Vec::new()),
            BuiltinValue::Some
            | BuiltinValue::Ok
            | BuiltinValue::Err
            | BuiltinValue::JsonNull
            | BuiltinValue::JsonBool
            | BuiltinValue::JsonNumber
            | BuiltinValue::JsonText
            | BuiltinValue::JsonArray
            | BuiltinValue::JsonObject
            | BuiltinValue::JsonInvalidSyntax
            | BuiltinValue::JsonNumberOutOfRange
            | BuiltinValue::JsonDepthLimit
            | BuiltinValue::JsonNonFiniteNumber
            | BuiltinValue::TaskCompleted
            | BuiltinValue::TaskFaulted
            | BuiltinValue::TaskCancelled => {
                let (enum_ty, variant) = match builtin {
                    BuiltinValue::Some => (OPTION_TYPE, VariantId(1)),
                    BuiltinValue::Ok => (RESULT_TYPE, VariantId(0)),
                    BuiltinValue::Err => (RESULT_TYPE, VariantId(1)),
                    BuiltinValue::JsonNull => (JSON_TYPE, VariantId(0)),
                    BuiltinValue::JsonBool => (JSON_TYPE, VariantId(1)),
                    BuiltinValue::JsonNumber => (JSON_TYPE, VariantId(2)),
                    BuiltinValue::JsonText => (JSON_TYPE, VariantId(3)),
                    BuiltinValue::JsonArray => (JSON_TYPE, VariantId(4)),
                    BuiltinValue::JsonObject => (JSON_TYPE, VariantId(5)),
                    BuiltinValue::JsonInvalidSyntax => (JSON_ERROR_TYPE, VariantId(0)),
                    BuiltinValue::JsonNumberOutOfRange => (JSON_ERROR_TYPE, VariantId(1)),
                    BuiltinValue::JsonDepthLimit => (JSON_ERROR_TYPE, VariantId(2)),
                    BuiltinValue::JsonNonFiniteNumber => (JSON_ERROR_TYPE, VariantId(3)),
                    BuiltinValue::TaskCompleted => (TASK_OUTCOME_TYPE, VariantId(0)),
                    BuiltinValue::TaskFaulted => (TASK_OUTCOME_TYPE, VariantId(1)),
                    BuiltinValue::TaskCancelled => (TASK_OUTCOME_TYPE, VariantId(2)),
                    _ => unreachable!(),
                };
                ExprKind::Variant {
                    ty: enum_ty,
                    type_arguments: Self::nominal_type_arguments(&ty, enum_ty, span)?,
                    variant,
                    payload: arguments
                        .iter()
                        .map(|argument| self.lower_expr(*argument))
                        .collect::<LowerResult<_>>()?,
                }
            }
            BuiltinValue::FloatParseStatus
            | BuiltinValue::FloatFormat
            | BuiltinValue::FloatIsFinite
            | BuiltinValue::IntToFloat
            | BuiltinValue::FloatToIntStatus
            | BuiltinValue::TextLength
            | BuiltinValue::TextGet
            | BuiltinValue::TextConcat
            | BuiltinValue::TextContains
            | BuiltinValue::TextEncodeUtf8
            | BuiltinValue::TextFromUtf8Units
            | BuiltinValue::BytesLength
            | BuiltinValue::BytesGet
            | BuiltinValue::BytesAdd
            | BuiltinValue::BytesAppend
            | BuiltinValue::BytesDecodeUtf8
            | BuiltinValue::PathFromText
            | BuiltinValue::PathAsText
            | BuiltinValue::PathJoin
            | BuiltinValue::TextMapNew
            | BuiltinValue::TextMapLength
            | BuiltinValue::TextMapContains
            | BuiltinValue::TextMapGet
            | BuiltinValue::TextMapEntryAt
            | BuiltinValue::TextMapInsert
            | BuiltinValue::ListToTextMap
            | BuiltinValue::TextMapRemove
            | BuiltinValue::IoErrorKind
            | BuiltinValue::IoErrorMessage
            | BuiltinValue::LogWrite
            | BuiltinValue::StdoutWrite
            | BuiltinValue::ListAdd
            | BuiltinValue::ListLength
            | BuiltinValue::ListGet
            | BuiltinValue::ProcessArgumentCount
            | BuiltinValue::ProcessArgumentAt
            | BuiltinValue::ProcessEnvironment
            | BuiltinValue::TaskFaultCode
            | BuiltinValue::TaskFaultMessage
            | BuiltinValue::DurationMilliseconds
            | BuiltinValue::DurationAsMilliseconds
            | BuiltinValue::FileOpenRead
            | BuiltinValue::FileCreate
            | BuiltinValue::FileTryOpenRead
            | BuiltinValue::FileTryCreate
            | BuiltinValue::FileReadText
            | BuiltinValue::FileWriteText
            | BuiltinValue::FileTryReadText
            | BuiltinValue::FileTryWriteText
            | BuiltinValue::FileClose
            | BuiltinValue::SocketConnect
            | BuiltinValue::SocketTryConnect
            | BuiltinValue::SocketReadText
            | BuiltinValue::SocketWriteText
            | BuiltinValue::SocketTryReadText
            | BuiltinValue::SocketTryWriteText
            | BuiltinValue::SocketClose => {
                let target = executable_builtin(builtin)
                    .ok_or_else(|| defect("non-executable builtin reached call lowering", span))?;
                let mut lowered =
                    Vec::with_capacity(arguments.len() + usize::from(receiver.is_some()));
                if let Some(receiver) = receiver {
                    match self.call(id)?.receiver {
                        Some(loom_sema::ReceiverPassing::InOut) => {
                            lowered.push(CallArgument::InOut(self.expression_place(receiver)?));
                        }
                        Some(loom_sema::ReceiverPassing::Value) => {
                            lowered.push(CallArgument::Value(
                                self.lower_expr_mode(receiver, ReadMode::MethodReceiver)?,
                            ));
                        }
                        None => {
                            return Err(defect(
                                "builtin method has no receiver passing mode",
                                span,
                            ));
                        }
                    }
                }
                lowered.extend(
                    arguments
                        .iter()
                        .map(|argument| self.lower_expr(*argument).map(CallArgument::Value))
                        .collect::<LowerResult<Vec<_>>>()?,
                );
                ExprKind::Call {
                    target: CallTarget::Builtin(target),
                    type_arguments: Vec::new(),
                    arguments: lowered,
                    witnesses: Vec::new(),
                }
            }
            BuiltinValue::Unit | BuiltinValue::None => {
                return Err(defect("non-callable builtin reached call lowering", span));
            }
        };
        Ok(Expr::new(kind, ty, span))
    }

    fn lower_call_type_arguments(
        &self,
        callable: DefId,
        scope: GenericArgumentScope,
        substitution: &loom_sema::Substitution,
        span: Span,
    ) -> LowerResult<Vec<Type>> {
        let Signature::Callable(signature) = self.compiler.signature(callable)? else {
            return Err(defect(
                "concept call target has no callable signature",
                span,
            ));
        };
        let parameters = match scope {
            GenericArgumentScope::All => &signature.generic_params,
            GenericArgumentScope::CallOnly => &signature.call_generic_params,
        };
        parameters
            .iter()
            .map(|parameter| {
                let ty = required(
                    substitution.get(*parameter),
                    "call substitution omits a callable type argument",
                    span,
                )?;
                self.compiler.lower_ty(ty, &self.parameters, span)
            })
            .collect()
    }

    fn nominal_type_arguments(ty: &Type, expected: TypeId, span: Span) -> LowerResult<Vec<Type>> {
        match ty {
            Type::Nominal(actual, arguments) if *actual == expected => Ok(arguments.clone()),
            Type::Nominal(result, arguments) if *result == RESULT_TYPE => {
                let Some(Type::Nominal(actual, nominal_arguments)) = arguments.first() else {
                    return Err(defect(
                        "checked construction result omits its nominal success type",
                        span,
                    ));
                };
                if *actual != expected {
                    return Err(defect(
                        "checked construction result names a different nominal success type",
                        span,
                    ));
                }
                Ok(nominal_arguments.clone())
            }
            _ => Err(defect(
                "construction expression type does not instantiate its nominal definition",
                span,
            )),
        }
    }

    fn lower_witness_selection(
        &self,
        selection: &WitnessSelection,
        span: Span,
    ) -> LowerResult<WitnessRef> {
        match selection.source {
            WitnessSource::Implementation(definition) => {
                let witness = required(
                    self.compiler.indices.witnesses.get(&definition).copied(),
                    "selected conformance has no MIR witness id",
                    span,
                )?;
                let is_generic = matches!(
                    &self.compiler.hir.definitions[definition].kind,
                    DefinitionKind::Conformance(conformance) if !conformance.generic_params.is_empty()
                );
                if selection.prerequisites.is_empty() && !is_generic {
                    Ok(WitnessRef::Concrete(witness))
                } else {
                    Ok(WitnessRef::Apply {
                        witness,
                        arguments: selection
                            .prerequisites
                            .iter()
                            .map(|prerequisite| self.lower_witness_selection(prerequisite, span))
                            .collect::<LowerResult<_>>()?,
                    })
                }
            }
            WitnessSource::ParamBound(index) => Ok(WitnessRef::Parameter(
                u32::try_from(index)
                    .map_err(|_| defect("witness parameter index exceeds u32", span))?,
            )),
        }
    }

    fn lower_witness_target(&self, selection: &WitnessSelection, span: Span) -> LowerResult<Type> {
        match selection.source {
            WitnessSource::Implementation(definition) => {
                let semantics = required(
                    self.compiler.analysis.typed.conformances.get(definition),
                    "selected conformance has no semantic target",
                    span,
                )?;
                let mut instantiated = self.parameters.clone();
                for (parameter, ty) in selection.substitution.iter() {
                    instantiated.substitutions.insert(
                        parameter,
                        self.compiler.lower_ty(ty, &self.parameters, span)?,
                    );
                }
                self.compiler
                    .lower_ty(semantics.target, &instantiated, span)
            }
            WitnessSource::ParamBound(index) => {
                let Signature::Callable(signature) = self.compiler.signature(self.body.owner)?
                else {
                    return Err(defect(
                        "witness-parameter call owner has no callable signature",
                        span,
                    ));
                };
                let bound = required(
                    signature.bounds.get(index),
                    "selected witness parameter is outside the callable proof list",
                    span,
                )?;
                self.compiler
                    .lower_ty(bound.self_ty, &self.parameters, span)
            }
        }
    }

    fn lower_place_read(&self, id: ExprId, mode: ReadMode) -> LowerResult<ExprKind> {
        let span = self.expr_span(id);
        let place = self.expression_place(id)?;
        let is_mutable_view = matches!(
            self.expression_semantic_ty(id)
                .map(|ty| self.compiler.analysis.typed.types.data(ty)),
            Ok(TyData::View {
                mutability: Mutability::Mutable,
                ..
            })
        );
        if self.semantics.view_moves.get(id).is_some() {
            if !is_mutable_view || mode != ReadMode::Value || !place.projection.is_empty() {
                return Err(defect(
                    "semantic interface access does not name a complete mutable interface value",
                    span,
                ));
            }
            return Ok(ExprKind::Move(place));
        }
        Ok(ExprKind::Copy(place))
    }

    #[allow(clippy::too_many_lines)]
    fn lower_record_literal(
        &mut self,
        id: ExprId,
        source_fields: &[loom_hir::RecordFieldValue],
    ) -> LowerResult<Expr> {
        let span = self.expr_span(id);
        let expression_ty = self.uncoerced_expression_ty(id)?;
        let canonical = required(
            self.semantics.record_fields.get(id),
            "record literal has no canonical semantic field mapping",
            span,
        )?;
        let definition = canonical
            .first()
            .and_then(
                |(field, _)| match &self.compiler.hir.definitions[*field].kind {
                    DefinitionKind::Field(field) => Some(field.owner),
                    _ => None,
                },
            )
            .or_else(|| {
                match self
                    .compiler
                    .analysis
                    .typed
                    .types
                    .data(self.expression_semantic_ty(id).ok()?)
                {
                    TyData::Nominal { definition, .. } => Some(*definition),
                    TyData::Result { ok, .. } => match self.compiler.analysis.typed.types.data(*ok)
                    {
                        TyData::Nominal { definition, .. } => Some(*definition),
                        _ => None,
                    },
                    _ => None,
                }
            });
        let definition = required(definition, "record literal has no nominal definition", span)?;
        let ty = required(
            self.compiler.indices.types.get(&definition).copied(),
            "record literal definition has no MIR type id",
            span,
        )?;

        let mut statements = Vec::with_capacity(source_fields.len());
        let mut temporaries = BTreeMap::new();
        for (index, field) in source_fields.iter().enumerate() {
            let value_ty = self.expression_ty(field.value)?;
            let local = self.add_temp(
                format!("$record_field_{index}"),
                value_ty.clone(),
                self.expr_span(field.value),
            )?;
            statements.push(Statement {
                kind: StatementKind::Let {
                    local,
                    value: self.lower_expr(field.value)?,
                },
                span: field.span,
            });
            temporaries.insert(field.value, (local, value_ty));
        }
        let fields = canonical
            .iter()
            .map(|(_, value)| {
                let (local, ty) = required(
                    temporaries.get(value),
                    "canonical record field is absent from source-order temporaries",
                    span,
                )?;
                Ok(Expr {
                    id: loom_mir::ExprId::UNASSIGNED,
                    kind: ExprKind::Copy(loom_mir::Place::local(*local)),
                    ty: ty.clone(),
                    span: self.expr_span(*value),
                })
            })
            .collect::<LowerResult<_>>()?;
        let construction = match &self.compiler.hir.definitions[definition].kind {
            DefinitionKind::Record(record) if record.invariant.is_some() => {
                match required(
                    self.semantics.construction_checks.get(id).copied(),
                    "invariant-bearing record has no proof disposition",
                    span,
                )? {
                    ConstructionCheck::Proven => ConstructionMode::Proven,
                    ConstructionCheck::Runtime => ConstructionMode::Runtime,
                }
            }
            DefinitionKind::Record(_) => ConstructionMode::Plain,
            _ => {
                return Err(defect("record literal definition is not a record", span));
            }
        };
        let record = Expr {
            id: loom_mir::ExprId::UNASSIGNED,
            kind: ExprKind::Record {
                ty,
                type_arguments: Self::nominal_type_arguments(&expression_ty, ty, span)?,
                fields,
                construction,
            },
            ty: expression_ty.clone(),
            span,
        };
        Ok(Expr {
            id: loom_mir::ExprId::UNASSIGNED,
            kind: ExprKind::Block(Block {
                statements,
                tail: Some(Box::new(record)),
                span,
            }),
            ty: expression_ty,
            span,
        })
    }

    fn add_temp(&mut self, name: String, ty: Type, span: Span) -> LowerResult<LocalId> {
        let id = LocalId(
            u32::try_from(self.params.len() + self.locals.len())
                .map_err(|_| defect("too many compiler-generated locals", span))?,
        );
        self.locals.push(LocalDecl {
            id,
            name,
            ty,
            mutable: false,
            span,
        });
        Ok(id)
    }

    fn expression_place(&self, id: ExprId) -> LowerResult<loom_mir::Place> {
        let span = self.expr_span(id);
        let place = required(
            self.semantics.expression_places.get(id),
            "place expression has no semantic place",
            span,
        )?;
        self.lower_place(place, span)
    }

    fn lower_place(&self, place: &SemaPlace, span: Span) -> LowerResult<loom_mir::Place> {
        let local = match place.root {
            PlaceRoot::Param(parameter) => required(
                self.param_locals.get(&parameter).copied(),
                "parameter place has no MIR local",
                span,
            )?,
            PlaceRoot::Local(local) => required(
                self.hir_locals.get(&local).copied(),
                "local place has no MIR local",
                span,
            )?,
            PlaceRoot::SelfValue => required(
                self.self_local,
                "self place reached a function without a receiver",
                span,
            )?,
        };
        let projection = place
            .projections
            .iter()
            .map(|projection| match projection {
                PlaceProjection::Field(field) => required(
                    self.compiler.indices.fields.get(field).copied(),
                    "semantic field projection has no MIR index",
                    span,
                ),
            })
            .collect::<LowerResult<_>>()?;
        Ok(loom_mir::Place { local, projection })
    }

    fn expression_ty(&self, id: ExprId) -> LowerResult<Type> {
        let span = self.expr_span(id);
        let semantic = self.expression_semantic_ty(id)?;
        self.compiler.lower_ty(semantic, &self.parameters, span)
    }

    fn uncoerced_expression_ty(&self, id: ExprId) -> LowerResult<Type> {
        if matches!(
            self.semantics.expression_coercions.get(id),
            Some(Coercion::ConcreteToDyn)
        ) {
            let view = required(
                self.semantics.views.get(id),
                "dynamic coercion has no selected witness",
                self.expr_span(id),
            )?;
            let ViewSource::Concrete { witness, .. } = &view.source else {
                return Err(defect(
                    "concrete dynamic coercion has no concrete witness",
                    self.expr_span(id),
                ));
            };
            return self.lower_witness_target(witness, self.expr_span(id));
        }
        self.expression_ty(id)
    }

    fn expression_semantic_ty(&self, id: ExprId) -> LowerResult<TyId> {
        required(
            self.semantics.expression_types.get(id).copied(),
            "expression has no semantic type",
            self.expr_span(id),
        )
    }

    fn call(&self, id: ExprId) -> LowerResult<&CallResolution> {
        required(
            self.semantics.calls.get(id),
            "call expression has no semantic call resolution",
            self.expr_span(id),
        )
    }

    fn expr_span(&self, id: ExprId) -> Span {
        self.body.source_map.expr(id).unwrap_or_default()
    }
}

fn executable_builtin(builtin: BuiltinValue) -> Option<Builtin> {
    Some(match builtin {
        BuiltinValue::FloatParseStatus => Builtin::FloatParseStatus,
        BuiltinValue::FloatFormat => Builtin::FloatFormat,
        BuiltinValue::FloatIsFinite => Builtin::FloatIsFinite,
        BuiltinValue::IntToFloat => Builtin::IntToFloat,
        BuiltinValue::FloatToIntStatus => Builtin::FloatToIntStatus,
        BuiltinValue::TextLength => Builtin::TextLength,
        BuiltinValue::TextGet => Builtin::TextGet,
        BuiltinValue::TextConcat => Builtin::TextConcat,
        BuiltinValue::TextContains => Builtin::TextContains,
        BuiltinValue::TextEncodeUtf8 => Builtin::TextEncodeUtf8,
        BuiltinValue::TextFromUtf8Units => Builtin::TextFromUtf8Units,
        BuiltinValue::BytesLength => Builtin::BytesLength,
        BuiltinValue::BytesGet => Builtin::BytesGet,
        BuiltinValue::BytesAdd => Builtin::BytesAdd,
        BuiltinValue::BytesAppend => Builtin::BytesAppend,
        BuiltinValue::BytesDecodeUtf8 => Builtin::BytesDecodeUtf8,
        BuiltinValue::PathFromText => Builtin::PathFromText,
        BuiltinValue::PathAsText => Builtin::PathAsText,
        BuiltinValue::PathJoin => Builtin::PathJoin,
        BuiltinValue::TextMapNew => Builtin::TextMapNew,
        BuiltinValue::TextMapLength => Builtin::TextMapLength,
        BuiltinValue::TextMapContains => Builtin::TextMapContains,
        BuiltinValue::TextMapGet => Builtin::TextMapGet,
        BuiltinValue::TextMapEntryAt => Builtin::TextMapEntryAt,
        BuiltinValue::TextMapInsert => Builtin::TextMapInsert,
        BuiltinValue::ListToTextMap => Builtin::ListToTextMap,
        BuiltinValue::TextMapRemove => Builtin::TextMapRemove,
        BuiltinValue::IoErrorKind => Builtin::IoErrorKind,
        BuiltinValue::IoErrorMessage => Builtin::IoErrorMessage,
        BuiltinValue::LogWrite => Builtin::LogWrite,
        BuiltinValue::StdoutWrite => Builtin::StdoutWrite,
        BuiltinValue::ListAdd => Builtin::ListAdd,
        BuiltinValue::ListLength => Builtin::ListLength,
        BuiltinValue::ListGet => Builtin::ListGet,
        BuiltinValue::ProcessArgumentCount => Builtin::ProcessArgumentCount,
        BuiltinValue::ProcessArgumentAt => Builtin::ProcessArgumentAt,
        BuiltinValue::ProcessEnvironment => Builtin::ProcessEnvironment,
        BuiltinValue::TaskFaultCode => Builtin::TaskFaultCode,
        BuiltinValue::TaskFaultMessage => Builtin::TaskFaultMessage,
        BuiltinValue::DurationMilliseconds => Builtin::DurationMilliseconds,
        BuiltinValue::DurationAsMilliseconds => Builtin::DurationAsMilliseconds,
        BuiltinValue::FileOpenRead => Builtin::FileOpenRead,
        BuiltinValue::FileCreate => Builtin::FileCreate,
        BuiltinValue::FileTryOpenRead => Builtin::FileTryOpenRead,
        BuiltinValue::FileTryCreate => Builtin::FileTryCreate,
        BuiltinValue::FileReadText => Builtin::FileReadText,
        BuiltinValue::FileWriteText => Builtin::FileWriteText,
        BuiltinValue::FileTryReadText => Builtin::FileTryReadText,
        BuiltinValue::FileTryWriteText => Builtin::FileTryWriteText,
        BuiltinValue::FileClose => Builtin::FileClose,
        BuiltinValue::SocketConnect => Builtin::SocketConnect,
        BuiltinValue::SocketTryConnect => Builtin::SocketTryConnect,
        BuiltinValue::SocketReadText => Builtin::SocketReadText,
        BuiltinValue::SocketWriteText => Builtin::SocketWriteText,
        BuiltinValue::SocketTryReadText => Builtin::SocketTryReadText,
        BuiltinValue::SocketTryWriteText => Builtin::SocketTryWriteText,
        BuiltinValue::SocketClose => Builtin::SocketClose,
        _ => return None,
    })
}

fn builtin_variant_id(builtin: BuiltinValue) -> Option<(TypeId, VariantId)> {
    Some(match builtin {
        BuiltinValue::None => (OPTION_TYPE, VariantId(0)),
        BuiltinValue::Some => (OPTION_TYPE, VariantId(1)),
        BuiltinValue::Ok => (RESULT_TYPE, VariantId(0)),
        BuiltinValue::Err => (RESULT_TYPE, VariantId(1)),
        BuiltinValue::JsonNull => (JSON_TYPE, VariantId(0)),
        BuiltinValue::JsonBool => (JSON_TYPE, VariantId(1)),
        BuiltinValue::JsonNumber => (JSON_TYPE, VariantId(2)),
        BuiltinValue::JsonText => (JSON_TYPE, VariantId(3)),
        BuiltinValue::JsonArray => (JSON_TYPE, VariantId(4)),
        BuiltinValue::JsonObject => (JSON_TYPE, VariantId(5)),
        BuiltinValue::JsonInvalidSyntax => (JSON_ERROR_TYPE, VariantId(0)),
        BuiltinValue::JsonNumberOutOfRange => (JSON_ERROR_TYPE, VariantId(1)),
        BuiltinValue::JsonDepthLimit => (JSON_ERROR_TYPE, VariantId(2)),
        BuiltinValue::JsonNonFiniteNumber => (JSON_ERROR_TYPE, VariantId(3)),
        BuiltinValue::TaskCompleted => (TASK_OUTCOME_TYPE, VariantId(0)),
        BuiltinValue::TaskFaulted => (TASK_OUTCOME_TYPE, VariantId(1)),
        BuiltinValue::TaskCancelled => (TASK_OUTCOME_TYPE, VariantId(2)),
        _ => return None,
    })
}

fn contract_parameter_indices(
    program: &HirProgram,
    owner: DefId,
) -> LowerResult<BTreeMap<ParamId, u32>> {
    let signature = match &program.definitions[owner].kind {
        DefinitionKind::Function(function) | DefinitionKind::Test(function) => &function.signature,
        DefinitionKind::Method(method) => &method.signature,
        DefinitionKind::Constant(_)
        | DefinitionKind::RefinedType(_)
        | DefinitionKind::Record(_)
        | DefinitionKind::Enum(_)
        | DefinitionKind::Field(_)
        | DefinitionKind::Variant(_)
        | DefinitionKind::InherentImpl(_)
        | DefinitionKind::Concept(_)
        | DefinitionKind::AssociatedType(_)
        | DefinitionKind::Conformance(_)
        | DefinitionKind::Error => return Ok(BTreeMap::new()),
    };
    signature
        .params
        .iter()
        .copied()
        .enumerate()
        .map(|(index, parameter)| {
            Ok((
                parameter,
                u32::try_from(index).map_err(|_| {
                    defect(
                        "contract has too many parameters",
                        program.source_map.param(parameter).unwrap_or_default(),
                    )
                })?,
            ))
        })
        .collect()
}

fn callable_source(
    program: &HirProgram,
    owner: DefId,
) -> LowerResult<&loom_hir::CallableSignature> {
    match &program.definitions[owner].kind {
        DefinitionKind::Function(function) | DefinitionKind::Test(function) => {
            Ok(&function.signature)
        }
        DefinitionKind::Method(method) => Ok(&method.signature),
        _ => Err(defect(
            "contract owner is not callable",
            definition_span(program, owner),
        )),
    }
}

fn body_type_parameters(
    program: &HirProgram,
    typed: &loom_sema::TypedProgram,
    owner: DefId,
) -> LowerResult<TypeParameters> {
    match &program.definitions[owner].kind {
        DefinitionKind::Record(record) => TypeParameters::from_ids(&record.generic_params),
        DefinitionKind::Enum(enumeration) => TypeParameters::from_ids(&enumeration.generic_params),
        DefinitionKind::Function(_) | DefinitionKind::Test(_) | DefinitionKind::Method(_) => {
            let Some(Signature::Callable(signature)) = typed.signatures.get(owner) else {
                return Err(defect(
                    "contract callable has no semantic signature",
                    definition_span(program, owner),
                ));
            };
            TypeParameters::from_callable(signature)
        }
        DefinitionKind::Constant(_) | DefinitionKind::RefinedType(_) => {
            Ok(TypeParameters::default())
        }
        _ => Err(defect(
            "contract body owner cannot provide generic parameters",
            definition_span(program, owner),
        )),
    }
}

#[derive(Clone, Default)]
struct TypeParameters {
    by_id: BTreeMap<GenericParamId, u32>,
    substitutions: BTreeMap<GenericParamId, Type>,
    projection_witnesses: BTreeMap<(TyId, DefId), u32>,
    self_types: BTreeMap<DefId, Type>,
    associated_types: BTreeMap<(DefId, DefId), Type>,
}

impl TypeParameters {
    fn from_ids(ids: &[GenericParamId]) -> LowerResult<Self> {
        let mut by_id = BTreeMap::new();
        for (index, parameter) in ids.iter().copied().enumerate() {
            by_id.insert(
                parameter,
                u32::try_from(index)
                    .map_err(|_| defect("too many generic parameters", Span::default()))?,
            );
        }
        Ok(Self {
            by_id,
            substitutions: BTreeMap::new(),
            projection_witnesses: BTreeMap::new(),
            self_types: BTreeMap::new(),
            associated_types: BTreeMap::new(),
        })
    }

    fn from_callable(signature: &loom_sema::CallableSignature) -> LowerResult<Self> {
        let mut result = Self::from_ids(&signature.generic_params)?;
        for (index, bound) in signature.bounds.iter().enumerate() {
            result.projection_witnesses.insert(
                (bound.self_ty, bound.concept.concept),
                u32::try_from(index)
                    .map_err(|_| defect("too many callable witness parameters", Span::default()))?,
            );
        }
        Ok(result)
    }

    fn index(&self, parameter: GenericParamId, span: Span) -> LowerResult<u32> {
        required(
            self.by_id.get(&parameter).copied(),
            "semantic type references a generic parameter outside this declaration",
            span,
        )
    }

    fn parameter_type(&self, parameter: GenericParamId, span: Span) -> LowerResult<Type> {
        if let Some(ty) = self.substitutions.get(&parameter) {
            return Ok(ty.clone());
        }
        Ok(Type::Parameter(self.index(parameter, span)?))
    }

    fn self_type(&self, concept: DefId, span: Span) -> LowerResult<Type> {
        required(
            self.self_types.get(&concept).cloned(),
            "uninstantiated concept Self reached executable MIR type lowering",
            span,
        )
    }

    fn associated_type(&self, concept: DefId, associated: DefId) -> Option<Type> {
        self.associated_types.get(&(concept, associated)).cloned()
    }

    fn projection_witness(&self, self_ty: TyId, concept: DefId, span: Span) -> LowerResult<u32> {
        required(
            self.projection_witnesses.get(&(self_ty, concept)).copied(),
            "associated projection has no matching callable witness parameter",
            span,
        )
    }

    fn len(&self) -> u32 {
        u32::try_from(self.by_id.len()).expect("generic parameter count was checked")
    }
}

#[allow(clippy::too_many_lines)]
fn synthetic_types() -> Vec<TypeDef> {
    let span = Span::default();
    vec![
        TypeDef {
            id: OPTION_TYPE,
            name: "Option".into(),
            span,
            type_parameters: 1,
            kind: TypeDefKind::Enum {
                variants: vec![
                    VariantDef {
                        id: VariantId(0),
                        name: "None".into(),
                        payload: Vec::new(),
                        span,
                    },
                    VariantDef {
                        id: VariantId(1),
                        name: "Some".into(),
                        payload: vec![Type::Parameter(0)],
                        span,
                    },
                ],
            },
        },
        TypeDef {
            id: RESULT_TYPE,
            name: "Result".into(),
            span,
            type_parameters: 2,
            kind: TypeDefKind::Enum {
                variants: vec![
                    VariantDef {
                        id: VariantId(0),
                        name: "Ok".into(),
                        payload: vec![Type::Parameter(0)],
                        span,
                    },
                    VariantDef {
                        id: VariantId(1),
                        name: "Err".into(),
                        payload: vec![Type::Parameter(1)],
                        span,
                    },
                ],
            },
        },
        TypeDef {
            id: CONSTRAINT_ERROR_TYPE,
            name: "ConstraintError".into(),
            span,
            type_parameters: 0,
            kind: TypeDefKind::Record {
                fields: vec![
                    FieldDef {
                        name: "target_type".into(),
                        ty: Type::Text,
                        span,
                    },
                    FieldDef {
                        name: "code".into(),
                        ty: Type::Text,
                        span,
                    },
                    FieldDef {
                        name: "predicate".into(),
                        ty: Type::Text,
                        span,
                    },
                    FieldDef {
                        name: "path".into(),
                        ty: Type::List(Box::new(Type::Text)),
                        span,
                    },
                    FieldDef {
                        name: "value_summary".into(),
                        ty: Type::Text,
                        span,
                    },
                    FieldDef {
                        name: "contract_span".into(),
                        ty: Type::Tuple(vec![Type::Int, Type::Int, Type::Int]),
                        span,
                    },
                ],
                invariant: None,
            },
        },
        TypeDef {
            id: CONTRACT_FAULT_TYPE,
            name: "ContractFault".into(),
            span,
            type_parameters: 0,
            kind: TypeDefKind::Record {
                fields: Vec::new(),
                invariant: None,
            },
        },
        task_fault_type(span),
        task_outcome_type(span),
        opaque_record_type(DURATION_TYPE, "Duration", Type::Int, span),
        opaque_record_type(BYTES_TYPE, "Bytes", Type::Text, span),
        opaque_record_type(PATH_TYPE, "Path", Type::Text, span),
        TypeDef {
            id: TEXT_MAP_TYPE,
            name: "TextMap".into(),
            span,
            type_parameters: 1,
            kind: TypeDefKind::Record {
                fields: vec![FieldDef {
                    name: "raw".into(),
                    ty: Type::Int,
                    span,
                }],
                invariant: None,
            },
        },
        json_type(span),
        json_error_type(span),
    ]
}

fn json_type(span: Span) -> TypeDef {
    TypeDef {
        id: JSON_TYPE,
        name: "Json".into(),
        span,
        type_parameters: 0,
        kind: TypeDefKind::Enum {
            variants: vec![
                VariantDef {
                    id: VariantId(0),
                    name: "Null".into(),
                    payload: Vec::new(),
                    span,
                },
                VariantDef {
                    id: VariantId(1),
                    name: "Bool".into(),
                    payload: vec![Type::Bool],
                    span,
                },
                VariantDef {
                    id: VariantId(2),
                    name: "Number".into(),
                    payload: vec![Type::Float],
                    span,
                },
                VariantDef {
                    id: VariantId(3),
                    name: "Text".into(),
                    payload: vec![Type::Text],
                    span,
                },
                VariantDef {
                    id: VariantId(4),
                    name: "Array".into(),
                    payload: vec![Type::List(Box::new(Type::Nominal(JSON_TYPE, Vec::new())))],
                    span,
                },
                VariantDef {
                    id: VariantId(5),
                    name: "Object".into(),
                    payload: vec![Type::Nominal(
                        TEXT_MAP_TYPE,
                        vec![Type::Nominal(JSON_TYPE, Vec::new())],
                    )],
                    span,
                },
            ],
        },
    }
}

fn json_error_type(span: Span) -> TypeDef {
    TypeDef {
        id: JSON_ERROR_TYPE,
        name: "JsonError".into(),
        span,
        type_parameters: 0,
        kind: TypeDefKind::Enum {
            variants: [
                ("InvalidSyntax", vec![Type::Int]),
                ("NumberOutOfRange", vec![Type::Int]),
                ("DepthLimit", Vec::new()),
                ("NonFiniteNumber", Vec::new()),
            ]
            .into_iter()
            .enumerate()
            .map(|(index, (name, payload))| VariantDef {
                id: VariantId(u32::try_from(index).expect("json error variant index")),
                name: name.into(),
                payload,
                span,
            })
            .collect(),
        },
    }
}

fn canonical_resource_type(
    hir: &HirProgram,
    indices: &Indices,
    definition: Option<DefId>,
    qualified_name: &'static str,
) -> LowerResult<(DefId, TypeId)> {
    let definition = required(
        definition,
        format!("embedded {qualified_name} is required for MIR lowering"),
        Span::default(),
    )?;
    let span = definition_span(hir, definition);
    let source = &hir.definitions[definition];
    let DefinitionKind::Record(record) = &source.kind else {
        return Err(defect(
            format!("canonical {qualified_name} must be an empty source record"),
            span,
        ));
    };
    if source.visibility != Visibility::Public
        || !record.generic_params.is_empty()
        || !record.fields.is_empty()
        || record.invariant.is_some()
    {
        return Err(defect(
            format!(
                "canonical {qualified_name} must be a public empty non-generic record without an invariant"
            ),
            span,
        ));
    }
    let ty = required(
        indices.types.get(&definition).copied(),
        format!("canonical {qualified_name} has no MIR type id"),
        span,
    )?;
    Ok((definition, ty))
}

fn io_error_type(id: TypeId, kind: TypeId, span: Span) -> TypeDef {
    TypeDef {
        id,
        name: "IoError".into(),
        span,
        type_parameters: 0,
        kind: TypeDefKind::Record {
            fields: vec![
                FieldDef {
                    name: "kind".into(),
                    ty: Type::Nominal(kind, Vec::new()),
                    span,
                },
                FieldDef {
                    name: "message".into(),
                    ty: Type::Text,
                    span,
                },
            ],
            invariant: None,
        },
    }
}

fn lower_builtin_type(builtin: BuiltinType) -> Type {
    match builtin {
        BuiltinType::Bool => Type::Bool,
        BuiltinType::Int => Type::Int,
        BuiltinType::Float => Type::Float,
        BuiltinType::Text => Type::Text,
        BuiltinType::Bytes => Type::Nominal(BYTES_TYPE, Vec::new()),
        BuiltinType::Path => Type::Nominal(PATH_TYPE, Vec::new()),
        BuiltinType::Unit => Type::Unit,
        BuiltinType::ConstraintError => Type::Nominal(CONSTRAINT_ERROR_TYPE, Vec::new()),
        BuiltinType::ContractFault => Type::Nominal(CONTRACT_FAULT_TYPE, Vec::new()),
        BuiltinType::TaskFault => Type::Nominal(TASK_FAULT_TYPE, Vec::new()),
        BuiltinType::Duration => Type::Nominal(DURATION_TYPE, Vec::new()),
        BuiltinType::Json => Type::Nominal(JSON_TYPE, Vec::new()),
        BuiltinType::JsonError => Type::Nominal(JSON_ERROR_TYPE, Vec::new()),
    }
}

fn opaque_record_type(id: TypeId, name: &str, field: Type, span: Span) -> TypeDef {
    TypeDef {
        id,
        name: name.into(),
        span,
        type_parameters: 0,
        kind: TypeDefKind::Record {
            fields: vec![FieldDef {
                name: "raw".into(),
                ty: field,
                span,
            }],
            invariant: None,
        },
    }
}

fn task_fault_type(span: Span) -> TypeDef {
    TypeDef {
        id: TASK_FAULT_TYPE,
        name: "TaskFault".into(),
        span,
        type_parameters: 0,
        kind: TypeDefKind::Record {
            fields: vec![
                FieldDef {
                    name: "code".into(),
                    ty: Type::Text,
                    span,
                },
                FieldDef {
                    name: "message".into(),
                    ty: Type::Text,
                    span,
                },
            ],
            invariant: None,
        },
    }
}

fn task_outcome_type(span: Span) -> TypeDef {
    TypeDef {
        id: TASK_OUTCOME_TYPE,
        name: "TaskOutcome".into(),
        span,
        type_parameters: 1,
        kind: TypeDefKind::Enum {
            variants: vec![
                VariantDef {
                    id: VariantId(0),
                    name: "Completed".into(),
                    payload: vec![Type::Parameter(0)],
                    span,
                },
                VariantDef {
                    id: VariantId(1),
                    name: "Faulted".into(),
                    payload: vec![Type::Nominal(TASK_FAULT_TYPE, Vec::new())],
                    span,
                },
                VariantDef {
                    id: VariantId(2),
                    name: "Cancelled".into(),
                    payload: Vec::new(),
                    span,
                },
            ],
        },
    }
}

fn executable_body(kind: &DefinitionKind) -> Option<BodyId> {
    match kind {
        DefinitionKind::Function(function) | DefinitionKind::Test(function) => Some(function.body),
        DefinitionKind::Method(method) => method.body,
        _ => None,
    }
}

fn definition_span(program: &HirProgram, definition: DefId) -> Span {
    program
        .source_map
        .definition(definition)
        .unwrap_or_default()
}

fn definition_name(program: &HirProgram, definition: DefId) -> LowerResult<String> {
    required(
        program.definitions[definition]
            .name
            .as_ref()
            .map(ToString::to_string),
        "executable definition has no source name",
        definition_span(program, definition),
    )
}

fn required<T>(value: Option<T>, message: impl Into<String>, span: Span) -> LowerResult<T> {
    value.ok_or_else(|| defect(message, span))
}

fn lower_pattern(
    compiler: &Compiler<'_>,
    body: &Body,
    semantics: &BodySemantics,
    id: PatternId,
    locals: &BTreeMap<HirLocalId, LocalId>,
    bindings: &mut Vec<LocalId>,
) -> LowerResult<Pattern> {
    let span = body.source_map.pattern(id).unwrap_or_default();
    let source = &body.patterns[id];
    match source {
        loom_hir::Pattern::Error => Err(defect("error pattern reached MIR lowering", span)),
        loom_hir::Pattern::Wildcard => Ok(Pattern::Wildcard),
        loom_hir::Pattern::Binding(local) => {
            bindings.push(required(
                locals.get(local).copied(),
                "pattern binding has no MIR local",
                span,
            )?);
            Ok(Pattern::Binding)
        }
        loom_hir::Pattern::Literal(literal) => Ok(Pattern::Constant(lower_literal(literal, span)?)),
        loom_hir::Pattern::Name { payload, .. } | loom_hir::Pattern::Variant { payload, .. } => {
            let resolution = required(
                semantics.pattern_resolutions.get(id).copied(),
                "name pattern has no semantic resolution",
                span,
            )?;
            match resolution {
                Resolution::Local(local) => {
                    if !payload.is_empty() {
                        return Err(defect("binding pattern unexpectedly has payload", span));
                    }
                    bindings.push(required(
                        locals.get(&local).copied(),
                        "resolved pattern binding has no MIR local",
                        span,
                    )?);
                    Ok(Pattern::Binding)
                }
                Resolution::Definition(variant) => {
                    let (ty, variant) = required(
                        compiler.indices.variants.get(&variant).copied(),
                        "resolved user variant has no MIR variant id",
                        span,
                    )?;
                    Ok(Pattern::Variant {
                        ty,
                        variant,
                        payload: payload
                            .iter()
                            .map(|child| {
                                lower_pattern(compiler, body, semantics, *child, locals, bindings)
                            })
                            .collect::<LowerResult<_>>()?,
                    })
                }
                Resolution::Builtin(builtin) => {
                    let (ty, variant) = builtin_variant_id(builtin)
                        .ok_or_else(|| defect("non-variant builtin resolved as a pattern", span))?;
                    Ok(Pattern::Variant {
                        ty,
                        variant,
                        payload: payload
                            .iter()
                            .map(|child| {
                                lower_pattern(compiler, body, semantics, *child, locals, bindings)
                            })
                            .collect::<LowerResult<_>>()?,
                    })
                }
                _ => Err(defect(
                    "unsupported semantic pattern resolution reached MIR lowering",
                    span,
                )),
            }
        }
    }
}

fn lower_literal(literal: &Literal, span: Span) -> LowerResult<Constant> {
    match literal {
        Literal::Bool(value) => Ok(Constant::Bool(*value)),
        Literal::Int(value) => value
            .parse::<i64>()
            .map(Constant::Int)
            .map_err(|_| defect("checked Int literal could not be decoded", span)),
        Literal::Float(value) => value
            .parse::<f64>()
            .map(Constant::Float)
            .map_err(|_| defect("checked Float literal could not be decoded", span)),
        Literal::Text(value) => decode_text_literal(value)
            .map(Constant::Text)
            .map_err(|message| defect(message, span)),
        Literal::Unit => Ok(Constant::Unit),
    }
}

fn lower_constant_value(value: &ConstantValue) -> Constant {
    match value {
        ConstantValue::Bool(value) => Constant::Bool(*value),
        ConstantValue::Int(value) => Constant::Int(*value),
        ConstantValue::Float(value) => Constant::Float(*value),
        ConstantValue::Text(value) => Constant::Text(value.clone()),
    }
}

fn decode_text_literal(source: &str) -> Result<String, String> {
    let Some(inner) = source
        .strip_prefix('"')
        .and_then(|source| source.strip_suffix('"'))
    else {
        return Err("checked Text literal is missing delimiters".into());
    };
    let mut result = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\\' {
            result.push(character);
            continue;
        }
        let escape = chars
            .next()
            .ok_or_else(|| "checked Text literal ends inside an escape".to_owned())?;
        match escape {
            '"' => result.push('"'),
            '\\' => result.push('\\'),
            '/' => result.push('/'),
            'b' => result.push('\u{0008}'),
            'f' => result.push('\u{000c}'),
            'n' => result.push('\n'),
            'r' => result.push('\r'),
            't' => result.push('\t'),
            '0' => result.push('\0'),
            'u' => result.push(decode_unicode_escape(&mut chars)?),
            _ => return Err("checked Text literal contains an unknown escape".into()),
        }
    }
    Ok(result)
}

fn decode_unicode_escape<I>(chars: &mut std::iter::Peekable<I>) -> Result<char, String>
where
    I: Iterator<Item = char>,
{
    if chars.next_if_eq(&'{').is_some() {
        let mut digits = String::new();
        loop {
            let character = chars
                .next()
                .ok_or_else(|| "checked Unicode escape is unterminated".to_owned())?;
            if character == '}' {
                break;
            }
            digits.push(character);
        }
        let scalar = u32::from_str_radix(&digits, 16)
            .map_err(|_| "checked Unicode escape could not be decoded".to_owned())?;
        return char::from_u32(scalar)
            .ok_or_else(|| "checked Unicode escape is not a scalar".to_owned());
    }

    let mut first = 0_u16;
    for _ in 0..4 {
        let digit = chars
            .next()
            .and_then(|character| character.to_digit(16))
            .ok_or_else(|| "checked Unicode escape has an invalid fixed form".to_owned())?;
        first = (first << 4) | u16::try_from(digit).expect("hex digit fits u16");
    }
    if !(0xd800..=0xdfff).contains(&first) {
        return char::from_u32(u32::from(first))
            .ok_or_else(|| "checked Unicode escape is not a scalar".to_owned());
    }
    if !(0xd800..=0xdbff).contains(&first)
        || chars.next() != Some('\\')
        || chars.next() != Some('u')
    {
        return Err("checked Unicode surrogate pair is malformed".into());
    }
    let mut second = 0_u16;
    for _ in 0..4 {
        let digit = chars
            .next()
            .and_then(|character| character.to_digit(16))
            .ok_or_else(|| "checked Unicode surrogate pair is malformed".to_owned())?;
        second = (second << 4) | u16::try_from(digit).expect("hex digit fits u16");
    }
    if !(0xdc00..=0xdfff).contains(&second) {
        return Err("checked Unicode surrogate pair is malformed".into());
    }
    let scalar = 0x1_0000 + ((u32::from(first) - 0xd800) << 10) + (u32::from(second) - 0xdc00);
    char::from_u32(scalar).ok_or_else(|| "checked Unicode surrogate is not a scalar".to_owned())
}

const fn lower_unary(operator: HirUnaryOp) -> UnaryOp {
    match operator {
        HirUnaryOp::Negate => UnaryOp::Negate,
        HirUnaryOp::Not => UnaryOp::Not,
    }
}

fn build_balanced_logical_expression(
    operator: BinaryOp,
    mut operands: Vec<Expr>,
    fallback_span: Span,
) -> LowerResult<Expr> {
    if operands.is_empty() {
        return Err(defect("logical expression has no operands", fallback_span));
    }

    while operands.len() > 1 {
        let mut next = Vec::with_capacity(operands.len().div_ceil(2));
        let mut current = operands.into_iter();
        while let Some(left) = current.next() {
            let Some(right) = current.next() else {
                next.push(left);
                break;
            };
            let span = if left.span.file == right.span.file {
                Span::new(
                    left.span.file,
                    left.span.range.start.min(right.span.range.start),
                    left.span.range.end.max(right.span.range.end),
                )
            } else {
                fallback_span
            };
            next.push(Expr::new(
                ExprKind::Binary(operator, Box::new(left), Box::new(right)),
                Type::Bool,
                span,
            ));
        }
        operands = next;
    }

    operands
        .pop()
        .ok_or_else(|| defect("logical expression has no result", fallback_span))
}

fn build_balanced_logical_contract_expression(
    operator: BinaryOp,
    mut operands: Vec<ContractExpr>,
    fallback_span: Span,
) -> LowerResult<ContractExpr> {
    if operands.is_empty() {
        return Err(defect(
            "logical contract expression has no operands",
            fallback_span,
        ));
    }

    while operands.len() > 1 {
        let mut next = Vec::with_capacity(operands.len().div_ceil(2));
        let mut current = operands.into_iter();
        while let Some(left) = current.next() {
            let Some(right) = current.next() else {
                next.push(left);
                break;
            };
            let span = if left.span.file == right.span.file {
                Span::new(
                    left.span.file,
                    left.span.range.start.min(right.span.range.start),
                    left.span.range.end.max(right.span.range.end),
                )
            } else {
                fallback_span
            };
            next.push(ContractExpr {
                kind: ContractExprKind::Binary(operator, Box::new(left), Box::new(right)),
                span,
            });
        }
        operands = next;
    }

    operands
        .pop()
        .ok_or_else(|| defect("logical contract expression has no result", fallback_span))
}

const fn lower_binary(operator: HirBinaryOp) -> BinaryOp {
    match operator {
        HirBinaryOp::Add => BinaryOp::Add,
        HirBinaryOp::Subtract => BinaryOp::Subtract,
        HirBinaryOp::Multiply => BinaryOp::Multiply,
        HirBinaryOp::Divide => BinaryOp::Divide,
        HirBinaryOp::Equal => BinaryOp::Equal,
        HirBinaryOp::NotEqual => BinaryOp::NotEqual,
        HirBinaryOp::Less => BinaryOp::Less,
        HirBinaryOp::LessEqual => BinaryOp::LessEqual,
        HirBinaryOp::Greater => BinaryOp::Greater,
        HirBinaryOp::GreaterEqual => BinaryOp::GreaterEqual,
        HirBinaryOp::And => BinaryOp::And,
        HirBinaryOp::Or => BinaryOp::Or,
    }
}

#[cfg(test)]
mod tests {
    use super::decode_text_literal;

    #[test]
    fn text_decoder_handles_core_escape_forms() {
        assert_eq!(
            decode_text_literal(r#""line\n\u{1f680}\uD83D\uDE80\0""#).unwrap(),
            "line\n🚀🚀\0"
        );
    }

    #[test]
    fn text_decoder_refuses_non_scalar_recovery() {
        assert!(decode_text_literal(r#""\uD800x""#).is_err());
        assert!(decode_text_literal("missing quotes").is_err());
    }
}
