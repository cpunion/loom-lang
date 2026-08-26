//! Target-typed SSA code-generation IR for Loom.
//!
//! LCIR sits after checked, target-independent MIR and before an object-code
//! backend. Its job is to make physical value representations and control-flow
//! dataflow explicit before LLVM sees the program. It is compiler-private,
//! target-dependent, deliberately not serialized, and carries no compatibility
//! promise across compiler builds. Global IDs carry a private generative
//! program identity: equal raw table numbers from different programs are not
//! interchangeable, while the private identity stays out of textual output.
//!
//! This foundation intentionally supports only canonical scalar
//! representations and hand-built SSA graphs. MIR lowering and production
//! LLVM emission arrive as complete vertical slices rather than partial
//! universal-value fallbacks.
//! Block, instruction, and value identities are owned by one function; the
//! builder and independent validator never interpret another function's local
//! identity by its raw table index.

mod builder;
mod dump;
mod ids;
mod ir;
mod repr;
mod validate;

pub use builder::{BuildError, BuildErrorCode, FunctionBuilder, ProgramBuilder};
pub use dump::{DumpOptions, dump_program, write_program, write_program_with_options};
pub use ids::{BlockId, InstanceId, InstructionId, ReprId, ValueId, ValueTypeId};
pub use ir::{
    Block, BlockTarget, Constant, Effects, FaultCode, FloatBinaryOp, FloatPredicate, Function,
    Instruction, InstructionKind, IntPredicate, Origin, Program, Signature, Terminator,
    TerminatorKind, Value, ValueDefinition,
};
pub use repr::{Repr, RepresentationPlan, ScalarRepr, TargetLayout, TargetLayoutError, ValueType};
pub use validate::{
    CheckedProgram, ValidationCode, ValidationError, ValidationErrors, check_program,
    validate_program,
};
