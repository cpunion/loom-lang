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
//! This foundation intentionally supports only canonical scalar
//! representations and hand-built SSA graphs. MIR lowering and production
//! LLVM emission arrive as complete vertical slices rather than partial
//! universal-value fallbacks.
//! Block, instruction, and value identities are owned by one function; the
//! builder and independent validator never interpret another function's local
//! identity by its raw table index.

mod artifact;
mod artifact_identity;
mod builder;
mod dump;
mod ids;
mod ir;
mod repr;
mod source_graph;
mod validate;

pub use artifact::{
    ArtifactKind, ArtifactRootRequest, ArtifactValidationCode, ArtifactValidationError,
    ArtifactValidationErrors, CheckedArtifact, check_artifact, validate_artifact_roots,
};
pub use artifact_identity::{
    ARTIFACT_IDENTITY_ROUTE, ARTIFACT_IDENTITY_SCHEMA, artifact_identity, write_artifact_identity,
};
pub use builder::{BuildError, BuildErrorCode, FunctionBuilder, ProgramBuilder};
pub use dump::{DumpOptions, dump_program, write_program, write_program_with_options};
pub use ids::{BlockId, InstanceId, InstructionId, ReprId, ValueId, ValueTypeId};
pub use ir::{
    Block, BlockTarget, BoolPredicate, CheckedIntBinaryOp, Constant, Effects, FaultCode,
    FloatBinaryOp, FloatPredicate, Function, Instruction, InstructionKind, IntPredicate, Origin,
    Program, ResultTarget, Signature, Terminator, TerminatorKind, UnwindTarget, Value,
    ValueDefinition,
};
pub use repr::{Repr, RepresentationPlan, ScalarRepr, TargetLayout, TargetLayoutError, ValueType};
pub use source_graph::{
    GraphError, GraphErrorCode, ReachableSourceGraph, SourceRoots, analyze_source_reachability,
};
pub use validate::{
    CheckedProgram, ValidationCode, ValidationError, ValidationErrors, check_program,
    validate_program,
};
