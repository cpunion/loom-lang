//! LLVM object-code backend for checked Loom MIR.
//!
//! The backend consumes only validated MIR. Source/HIR concepts do not leak
//! into this layer: static calls are direct edges and erased calls use the
//! compiler-private `{ data, witness }` ABI described by [`abi`].

mod abi;
mod codegen;
mod emitter;
mod error;
mod reachability;
mod target;

pub use codegen::{
    DebugSource, EmitKind, EmitOptions, NativeArtifact, NativeObjectArtifact, emit_native,
    emit_native_object, link_native_object, native_object_fingerprint,
};
pub use error::CodegenError;
pub use reachability::{ReachableProgram, Roots, analyze_reachability};
pub use target::{
    CPU_FEATURES, CPU_POLICY, NATIVE_RUNTIME_ABI, NativeTargetIdentity, OPTIMIZATION_PIPELINE,
    RELOCATION_MODE, emit_native_debug_companion, materialize_native_debug_metadata,
    native_debug_companion_path, native_debug_tool_identity, native_linker_identity,
    native_runtime_identity, native_target_identity,
};

/// LLVM backend version recorded in diagnostics and future cache keys.
pub const BACKEND_VERSION: &str = env!("CARGO_PKG_VERSION");
