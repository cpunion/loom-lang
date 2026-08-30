#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;

#[cfg(unix)]
use std::os::fd::OwnedFd;
#[cfg(unix)]
use std::os::unix::net::UnixStream;

use loom_codegen_llvm::{
    CodegenError, EmitOptions, NativeArtifact, RuntimeBundle, RuntimeLinker,
    link_object_with_runtime_bundle, native_target_identity,
};
use loom_driver::{AnalysisHost, DriverError, ProjectOptions};
use loom_mir::CheckedProgram;

struct TestRuntime {
    bundle: RuntimeBundle,
    linker: RuntimeLinker,
}

static TEST_RUNTIME: OnceLock<TestRuntime> = OnceLock::new();

pub fn analysis_host(input: impl AsRef<Path>) -> Result<AnalysisHost, DriverError> {
    let input = input.as_ref();
    let source = if input.is_dir() {
        input.join("main.loom")
    } else {
        input.to_path_buf()
    };
    split_test_source(&source);
    AnalysisHost::new_with_options(
        input,
        &ProjectOptions {
            tests: loom_driver::TestSelection::Recursive,
            ..ProjectOptions::default()
        },
    )
}

fn split_test_source(path: &Path) {
    let Ok(source) = std::fs::read_to_string(path) else {
        return;
    };
    let Some(test_start) = test_source_start(&source) else {
        return;
    };
    let test_file_start = test_import_block_start(&source, test_start).unwrap_or(test_start);
    let (ordinary, tests) = source.split_at(test_file_start);
    std::fs::write(path, ordinary.trim_end()).expect("write ordinary source fixture");
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .expect("source fixture has a UTF-8 stem");
    let test_path = path.with_file_name(format!("{stem}_test.loom"));
    let imports = ordinary
        .lines()
        .filter(|line| line.starts_with("import "))
        .collect::<Vec<_>>()
        .join("\n");
    let test_source = if tests.trim_start().starts_with("import ") || imports.is_empty() {
        tests.to_owned()
    } else {
        format!("{imports}\n\n{tests}")
    };
    std::fs::write(test_path, test_source).expect("write test source fixture");
}

fn test_source_start(source: &str) -> Option<usize> {
    source
        .match_indices("test ")
        .find(|(offset, _)| {
            (*offset == 0 || source.as_bytes().get(offset - 1) == Some(&b'\n'))
                && (source[*offset..].starts_with("test fn ")
                    || source[*offset..].starts_with("test async fn "))
        })
        .map(|(offset, _)| offset)
}

fn test_import_block_start(source: &str, test_start: usize) -> Option<usize> {
    let mut offset = 0;
    let mut saw_declaration = false;
    for line in source[..test_start].split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with("import ") {
            if saw_declaration {
                return Some(offset);
            }
        } else if !trimmed.is_empty() {
            saw_declaration = true;
        }
        offset += line.len();
    }
    None
}

fn test_runtime() -> &'static TestRuntime {
    TEST_RUNTIME.get_or_init(|| {
        let root = std::env::var_os("LOOM_TEST_RUNTIME_BUNDLE")
            .or_else(|| std::env::var_os("LOOM_RUNTIME_BUNDLE"))
            .map_or_else(
                || {
                    panic!(
                        "native LLVM tests require LOOM_TEST_RUNTIME_BUNDLE or \
                     LOOM_RUNTIME_BUNDLE; prepare one with `loom runtime pack`"
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

pub fn loom_text_literal(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

pub fn link_native_object(object: &Path, output: &Path) -> Result<(), CodegenError> {
    let runtime = test_runtime();
    link_object_with_runtime_bundle(object, output, &runtime.bundle, &runtime.linker)
}

pub fn run_with_read_only_stdout(executable: &Path, directory: &Path) -> Output {
    let target = directory.join("read-only-stdout");
    std::fs::write(&target, b"stdout sentinel\n").expect("create stdout sentinel");
    let read_only = std::fs::File::open(&target).expect("open read-only stdout handle");
    let output = Command::new(executable)
        .stdout(Stdio::from(read_only))
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn executable with read-only stdout")
        .wait_with_output()
        .expect("wait for executable with read-only stdout");
    assert_eq!(
        std::fs::read(&target).expect("read stdout sentinel"),
        b"stdout sentinel\n",
        "child unexpectedly changed its read-only stdout target"
    );
    output
}

pub fn run_with_read_only_stderr(command: &mut Command, directory: &Path) -> Output {
    let target = directory.join("read-only-stderr");
    std::fs::write(&target, b"stderr sentinel\n").expect("create stderr sentinel");
    let read_only = std::fs::File::open(&target).expect("open read-only stderr handle");
    let output = command
        .stdout(Stdio::piped())
        .stderr(Stdio::from(read_only))
        .spawn()
        .expect("spawn executable with read-only stderr")
        .wait_with_output()
        .expect("wait for executable with read-only stderr");
    assert_eq!(
        std::fs::read(&target).expect("read stderr sentinel"),
        b"stderr sentinel\n",
        "child unexpectedly changed its read-only stderr target"
    );
    output
}

#[cfg(unix)]
pub fn run_with_closed_stdout(executable: &Path) -> Output {
    let (reader, writer) = UnixStream::pair().expect("create closed stdout socket pair");
    drop(reader);
    let writer = std::fs::File::from(OwnedFd::from(writer));
    Command::new(executable)
        .stdout(Stdio::from(writer))
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn executable with closed stdout")
        .wait_with_output()
        .expect("wait for executable with closed stdout")
}

#[cfg(unix)]
pub fn run_with_closed_stderr(command: &mut Command) -> Output {
    let (reader, writer) = UnixStream::pair().expect("create closed stderr socket pair");
    drop(reader);
    let writer = std::fs::File::from(OwnedFd::from(writer));
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::from(writer))
        .spawn()
        .expect("spawn executable with closed stderr")
        .wait_with_output()
        .expect("wait for executable with closed stderr")
}

pub fn runtime_bundle_identity() -> String {
    test_runtime().bundle.identity().to_owned()
}

pub fn runtime_bundle_root() -> PathBuf {
    test_runtime().bundle.root().to_path_buf()
}
