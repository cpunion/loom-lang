//! LLVM object-code backend for checked Loom MIR.
//!
//! The backend consumes only validated MIR. Source/HIR concepts do not leak
//! into this layer: static calls are direct edges and erased calls use the
//! compiler-private `{ data, witness }` ABI described by [`abi`].

mod abi;
mod codegen;
mod emitter;
mod error;
mod native_storage;
mod reachability;
mod requirements;
mod runtime_bundle;
mod target;

pub use codegen::{
    DebugSource, EmitKind, EmitOptions, NativeArtifact, NativeObjectArtifact, emit_native,
    emit_native_object, link_native_object, native_object_fingerprint, validate_native_link_target,
};
pub use error::CodegenError;
pub use reachability::{ReachableProgram, Roots, analyze_reachability};
pub use runtime_bundle::{
    RUNTIME_BUNDLE_MANIFEST, RUNTIME_BUNDLE_SCHEMA_VERSION, RuntimeBundle, RuntimeBundleExport,
    RuntimeLinker, export_native_runtime_bundle, link_object_with_runtime_bundle,
};
pub use target::{
    CPU_FEATURES, CPU_POLICY, DEVELOPMENT_OPTIMIZATION_PIPELINE, NATIVE_RUNTIME_ABI,
    NativeTargetIdentity, OptimizationProfile, RELEASE_OPTIMIZATION_PIPELINE, RELOCATION_MODE,
    emit_native_debug_companion, is_native_target, materialize_native_debug_metadata,
    native_debug_companion_path, native_debug_tool_identity, native_linker_identity,
    native_runtime_identity, native_target_identity, target_identity,
};

/// LLVM backend version recorded in diagnostics and future cache keys.
pub const BACKEND_VERSION: &str = env!("CARGO_PKG_VERSION");
