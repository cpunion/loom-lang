//! Source-graph planning and target-typed SSA code-generation IR for Loom.
//!
//! [`SourceRoots`] and [`ReachableSourceGraph`] close checked-MIR identities
//! before target-specific lowering. They are deterministic and serialized into
//! native object fingerprints. LCIR then sits between checked,
//! target-independent MIR and an object-code backend. Its job is to make
//! physical value representations and control-flow dataflow explicit before
//! LLVM sees the program. LCIR is compiler-private, target-dependent,
//! deliberately not serialized, and carries no compatibility promise across
//! compiler builds. Global IDs carry a private generative program identity:
//! equal raw table numbers from different programs are not interchangeable,
//! while the private identity stays out of textual output.
//!
//! The first whole-artifact lowerer intentionally supports only canonical
//! direct representations and reports every reachable unsupported MIR site
//! before constructing SSA. Production LLVM emission remains a separate
//! vertical slice; no LCIR function is mixed with a legacy function.
//! Block, instruction, and value identities are owned by one function; the
//! builder and independent validator never interpret another function's local
//! identity by its raw table index.

mod aggregate_plan;
mod artifact;
mod artifact_identity;
mod builder;
mod dump;
mod dyn_plan;
mod ids;
mod instance;
mod instance_closure;
mod ir;
mod lower;
mod managed_roots;
mod match_plan;
mod place_plan;
mod repr;
mod source_graph;
mod text_plan;
mod validate;

pub use artifact::{
    ArtifactKind, ArtifactRootRequest, ArtifactValidationCode, ArtifactValidationError,
    ArtifactValidationErrors, CheckedArtifact, TestOutcomePlan, check_artifact,
    validate_artifact_roots,
};
pub use artifact_identity::{
    ARTIFACT_IDENTITY_ROUTE, ARTIFACT_IDENTITY_SCHEMA, artifact_identity, write_artifact_identity,
};
pub use builder::{BuildError, BuildErrorCode, FunctionBuilder, ProgramBuilder};
pub use dump::{DumpOptions, dump_program, write_program, write_program_with_options};
pub use ids::{
    BlockId, InstanceId, InstructionId, ProductReprId, ReprId, SumReprId, ValueId, ValueTypeId,
};
pub use instance::{
    INSTANCE_KEY_STRUCTURE_BUDGET, InstanceKey, InstancePlan, InstanceRole,
    InstanceWitnessArgument, PlannedInstance,
};
pub use instance_closure::{INSTANCE_CLOSURE_MAX_CALL_EDGES, INSTANCE_CLOSURE_MAX_INSTANCES};
pub use ir::{
    Block, BlockTarget, BoolPredicate, CONTRACT_FAULT_TEXT_MAX_BYTES, CheckedIntBinaryOp, Constant,
    ContractFaultKind, ContractFaultMetadata, CoroutinePlan, CoroutineSuspension, Effects,
    FaultCode, FaultMetadata, FloatBinaryOp, FloatPredicate, Function, Instruction,
    InstructionKind, IntPredicate, LIST_LITERAL_MAX_ELEMENTS, Origin, Program, ResourceKind,
    ResultTarget, Signature, SumCase, Terminator, TerminatorKind, UnwindTarget, Value,
    ValueDefinition,
};
pub use lower::{
    InvalidRootCode, LoweringDefectCode, LoweringError, LoweringErrorCode, LoweringOutcome,
    ResourceLimitCode, SourceArtifactRequest, SupportReport, UnsupportedFeature, UnsupportedItem,
    lower_typed_artifact,
};
pub use managed_roots::{
    MANAGED_ROOT_MAX_CANDIDATE_SLOTS_PER_VALUE, ManagedRootPlan, ManagedRootProjection,
    ManagedRootSlot, ManagedSafepoint, plan_managed_roots,
};
pub use repr::{
    DynamicRepr, ProductRepr, Repr, RepresentationPlan, ScalarRepr, SumRepr, SumTagRepr,
    SumVariantRepr, TargetLayout, TargetLayoutError, TypeRegistration, ValueType, ValueTypeKind,
};
pub use source_graph::{
    GraphError, GraphErrorCode, ReachableSourceGraph, SourceRoots, analyze_source_reachability,
};
pub use text_plan::{TEXT_LITERAL_MAX_BYTES, TEXT_LITERAL_MAX_TOTAL_BYTES};
pub use validate::{
    CheckedProgram, ValidationCode, ValidationError, ValidationErrors, check_program,
    validate_program,
};
