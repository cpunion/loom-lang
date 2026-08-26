#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use loom_codegen_llvm::{
    CodegenError, EmitOptions, NativeArtifact, RuntimeBundle, RuntimeLinker,
    link_object_with_runtime_bundle, native_runtime_archive_name, native_target_identity,
    pack_native_runtime_bundle,
};
use loom_mir::CheckedProgram;

struct TestRuntime {
    _directory: tempfile::TempDir,
    bundle: RuntimeBundle,
    linker: RuntimeLinker,
}

static TEST_RUNTIME: OnceLock<TestRuntime> = OnceLock::new();

fn test_runtime() -> &'static TestRuntime {
    TEST_RUNTIME.get_or_init(|| {
        let executable = std::env::current_exe().expect("locate integration-test executable");
        let profile = executable
            .parent()
            .and_then(Path::parent)
            .expect("integration test is below the Cargo profile directory");
        let archive = profile.join(native_runtime_archive_name(None));
        assert!(
            archive.is_file(),
            "loom-runtime dev-dependency did not produce {}",
            archive.display()
        );
        let directory = tempfile::tempdir().expect("create test runtime bundle root");
        let root = directory.path().join("runtime");
        pack_native_runtime_bundle(&archive, &root).expect("pack test runtime bundle");
        let target = native_target_identity().expect("load host target identity");
        let bundle = RuntimeBundle::load(&root, &target).expect("load test runtime bundle");
        let linker =
            RuntimeLinker::load(std::env::var_os("LOOM_CC").unwrap_or_else(|| "clang".into()))
                .expect("load host linker");
        TestRuntime {
            _directory: directory,
            bundle,
            linker,
        }
    })
}

pub fn emit_native(
    program: &CheckedProgram,
    output: &Path,
    options: &EmitOptions,
) -> Result<NativeArtifact, CodegenError> {
    let runtime = test_runtime();
    loom_codegen_llvm::emit_native(program, output, options, &runtime.bundle, &runtime.linker)
}

pub fn link_native_object(object: &Path, output: &Path) -> Result<(), CodegenError> {
    let runtime = test_runtime();
    link_object_with_runtime_bundle(object, output, &runtime.bundle, &runtime.linker)
}

pub fn runtime_bundle_identity() -> String {
    test_runtime().bundle.identity().to_owned()
}

pub fn runtime_bundle_root() -> PathBuf {
    test_runtime().bundle.root().to_path_buf()
}
