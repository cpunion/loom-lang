//! LLVM object-code backends for checked Loom MIR and checked LCIR.
//!
//! The LCIR boundary emits its checked target-typed SSA directly. The
//! checked-MIR boundary remains separate: its erased calls use the
//! compiler-private `{ data, witness }` ABI described by [`abi`].

use std::sync::OnceLock;

use loom_mir::Builtin;

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

pub(crate) const fn builtin_requires_typed_io(builtin: Builtin) -> bool {
    matches!(
        builtin,
        Builtin::FileOpenRead
            | Builtin::FileCreate
            | Builtin::FileReadText
            | Builtin::FileWriteText
            | Builtin::FileClose
            | Builtin::SocketConnect
            | Builtin::SocketReadText
            | Builtin::SocketWriteText
            | Builtin::SocketClose
            | Builtin::FileTryOpenRead
            | Builtin::FileTryCreate
            | Builtin::FileTryReadText
            | Builtin::FileTryWriteText
            | Builtin::SocketTryConnect
            | Builtin::SocketTryReadText
            | Builtin::SocketTryWriteText
    )
}

/// Emits a bounded stage marker for diagnosing failures inside LLVM's C API.
///
/// Stage names are compiler-owned constants and deliberately exclude source
/// text, paths, environment values, and target-specific host details.
pub(crate) fn trace_llvm_stage(stage: &'static str) {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    if *ENABLED.get_or_init(|| std::env::var_os("LOOM_LLVM_TRACE_STAGES").is_some()) {
        eprintln!("loom LLVM stage: {stage}");
    }
}

#[cfg(test)]
mod typed_io_guard_tests {
    use loom_mir::Builtin;

    use super::builtin_requires_typed_io;

    #[test]
    fn every_file_and_socket_builtin_requires_typed_lcir() {
        for builtin in [
            Builtin::FileOpenRead,
            Builtin::FileCreate,
            Builtin::FileReadText,
            Builtin::FileWriteText,
            Builtin::FileClose,
            Builtin::SocketConnect,
            Builtin::SocketReadText,
            Builtin::SocketWriteText,
            Builtin::SocketClose,
            Builtin::FileTryOpenRead,
            Builtin::FileTryCreate,
            Builtin::FileTryReadText,
            Builtin::FileTryWriteText,
            Builtin::SocketTryConnect,
            Builtin::SocketTryReadText,
            Builtin::SocketTryWriteText,
        ] {
            assert!(
                builtin_requires_typed_io(builtin),
                "missing guard for {builtin:?}"
            );
        }
    }
}
