//! LLVM object-code backends for checked Loom MIR and checked LCIR.
//!
//! The LCIR boundary emits its checked target-typed SSA directly. The older
//! checked-MIR boundary remains separate: its erased calls use the
//! compiler-private `{ data, witness }` ABI described by [`abi`].

mod abi;
mod codegen;
mod emitter;
mod error;
mod lcir_emitter;
mod native_artifact;
mod native_layout;
mod native_link;
mod native_range;
mod native_storage;
mod prepared;
mod requirements;
mod runtime_bundle;
mod target;

pub use codegen::{
    DebugSource, EmitKind, EmitOptions, NativeArtifact, NativeObjectArtifact, NativeObjectOptions,
    emit_lcir_native_object, emit_native, emit_native_object, native_object_fingerprint,
};
pub use error::CodegenError;
pub use native_artifact::{
    NativeArtifactKind, native_artifact_extension, native_artifact_path,
    native_runtime_archive_name, target_uses_msvc_artifacts, target_uses_windows_artifacts,
};
pub use prepared::{
    NativePreparationError, NativePreparationErrorKind, NativeRouteKind, NativeRoutePolicy,
    PreparedNativeObject, emit_prepared_native_object, prepare_native_object,
    prepared_native_object_fingerprint, prepared_native_target_identity,
};
pub use runtime_bundle::{
    PackedRuntimeBundle, RUNTIME_BUNDLE_MANIFEST, RUNTIME_BUNDLE_SCHEMA_VERSION, RUNTIME_CPU,
    RUNTIME_CPU_FEATURES, RuntimeBundle, RuntimeLinker, link_object_with_runtime_bundle,
    pack_native_runtime_bundle,
};
pub use target::{
    DEVELOPMENT_OPTIMIZATION_PIPELINE, NATIVE_RUNTIME_ABI, NativeTargetIdentity,
    OptimizationProfile, RELEASE_OPTIMIZATION_PIPELINE, RELOCATION_MODE,
    emit_native_debug_companion, is_native_target, native_target_identity, target_identity,
};

/// LLVM backend version recorded in diagnostics and future cache keys.
pub const BACKEND_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Content identity of the exact backend sources, manifests, Rust cfg, and
/// linked LLVM build used to emit native objects.
pub const LLVM_OBJECT_BUILD_FINGERPRINT: &str = env!("LOOM_LLVM_OBJECT_BUILD_FINGERPRINT");
