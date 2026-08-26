#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use loom_codegen_llvm::{
    CodegenError, EmitOptions, NativeArtifact, RuntimeBundle, RuntimeLinker,
    link_object_with_runtime_bundle, native_target_identity,
};
use loom_mir::CheckedProgram;

struct TestRuntime {
    bundle: RuntimeBundle,
    linker: RuntimeLinker,
}

static TEST_RUNTIME: OnceLock<TestRuntime> = OnceLock::new();

fn test_runtime() -> &'static TestRuntime {
    TEST_RUNTIME.get_or_init(|| {
        let root = std::env::var_os("LOOM_TEST_RUNTIME_BUNDLE")
            .or_else(|| std::env::var_os("LOOM_RUNTIME_BUNDLE"))
            .map_or_else(
                || {
                    panic!(
                        "native LLVM tests require LOOM_TEST_RUNTIME_BUNDLE or \
                     LOOM_RUNTIME_BUNDLE; prepare one with `loomc runtime pack`"
                    )
                },
                PathBuf::from,
            );
        let target = native_target_identity().expect("load host target identity");
        let bundle = RuntimeBundle::load(&root, &target).expect("load test runtime bundle");
        let linker =
            RuntimeLinker::load(std::env::var_os("LOOM_CC").unwrap_or_else(|| "clang".into()))
                .expect("load host linker");
        TestRuntime { bundle, linker }
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
