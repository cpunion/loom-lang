use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

mod common;
use common::success;

fn package(root: &Path, name: &str, source: &str) -> PathBuf {
    let package = root.join(name);
    fs::create_dir(&package).unwrap();
    fs::write(package.join("main.loom"), source).unwrap();
    package
}

fn cached(mode: &str, package: &Path, cache: &Path) -> Command {
    let mut command = common::command(&[mode]);
    command
        .arg(package)
        .arg("--object-cache")
        .arg(cache)
        .env("LOOM_NATIVE_TIMINGS", "1")
        .env("LOOM_OPT_LEVEL", "2");
    command
}

fn cache_trace(output: &Output, hit: bool) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let expected = if hit { "hit" } else { "miss" };
    let unexpected = if hit { "miss" } else { "hit" };
    assert!(
        stderr.contains(&format!("loom cache: {expected}")),
        "{stderr}"
    );
    assert!(
        !stderr.contains(&format!("loom cache: {unexpected}")),
        "{stderr}"
    );
    assert_eq!(
        stderr.contains("loom-native phase: codegen"),
        !hit,
        "{stderr}"
    );
    assert!(stderr.contains("loom-native phase: link"), "{stderr}");
}

fn cached_success(output: &Output, hit: bool) {
    success(output);
    cache_trace(output, hit);
}

fn bundle(cache: &Path) -> PathBuf {
    let bundles: Vec<_> = fs::read_dir(cache.join("objects-v1"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "lobj")
        })
        .collect();
    assert_eq!(bundles.len(), 1, "{bundles:?}");
    bundles.into_iter().next().unwrap()
}

fn no_staging_directories(cache: &Path) {
    for entry in fs::read_dir(cache.join("objects-v1")).unwrap() {
        let entry = entry.unwrap();
        assert!(
            !entry.file_name().to_string_lossy().starts_with("stage-"),
            "temporary cache entry was not cleaned up: {:?}",
            entry.path()
        );
    }
}

fn managed_source(message: &str) -> String {
    format!(
        "import std.io.write_text\nimport std.text.concat\n\
         fn main() {{ discard write_text(concat({message:?}, \" 雪\\n\")) }}"
    )
}

#[test]
fn cache_reuses_objects_across_outputs_but_not_source_optimization_or_test_modes() {
    let directory = tempfile::tempdir().unwrap();
    // The explicitly selected cache may be created, but its parent already exists.
    let cache = directory.path().join("trusted cache 雪");
    let scalar = package(
        directory.path(),
        "scalar",
        "fn answer() Int { 42 }\nfn main() { assert answer() == 42 }\n\
         test fn answer_test() { assert answer() == 42 }",
    );
    let output = common::executable(directory.path(), "first-output");
    cached_success(
        &cached("build", &scalar, &cache)
            .arg("--output")
            .arg(&output)
            .output()
            .unwrap(),
        false,
    );
    success(&Command::new(&output).output().unwrap());
    fs::remove_file(&output).unwrap();
    // `run` selects a different output and must recreate/relink it from the object.
    cached_success(&cached("run", &scalar, &cache).output().unwrap(), true);
    assert!(common::executable(&scalar.join("target"), "main").is_file());
    for hit in [false, true] {
        let output = cached("test", &scalar, &cache).output().unwrap();
        cached_success(&output, hit);
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim(),
            "1 tests passed"
        );
    }
    cached_success(
        &cached("build", &scalar, &cache)
            .env("LOOM_OPT_LEVEL", "0")
            .output()
            .unwrap(),
        false,
    );

    let managed = package(directory.path(), "managed", &managed_source("first"));
    for hit in [false, true] {
        let output = cached("run", &managed, &cache).output().unwrap();
        cached_success(&output, hit);
        assert_eq!(String::from_utf8(output.stdout).unwrap(), "first 雪\n");
    }
    fs::write(managed.join("main.loom"), managed_source("changed")).unwrap();
    let output = cached("run", &managed, &cache).output().unwrap();
    cached_success(&output, false);
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "changed 雪\n");
}

#[test]
fn cache_checks_actual_bundle_bytes_and_never_bypasses_frontend_or_ir_emission() {
    let directory = tempfile::tempdir().unwrap();
    let cache = directory.path().join("cache");
    let source = "fn main() { assert 20 + 22 == 42 }";
    let package = package(directory.path(), "scalar", source);
    cached_success(&cached("run", &package, &cache).output().unwrap(), false);
    let object = bundle(&cache);
    for damage in 0..3 {
        let mut bytes = fs::read(&object).unwrap();
        assert!(!bytes.is_empty());
        match damage {
            0 => {
                // Five textual header lines precede the actual object payload;
                // corrupt that payload while leaving metadata and checksum intact.
                let payload = bytes
                    .iter()
                    .enumerate()
                    .filter(|(_, byte)| **byte == b'\n')
                    .nth(4)
                    .unwrap()
                    .0
                    + 1;
                assert!(payload < bytes.len() - 32);
                bytes[payload] ^= 1;
                fs::write(&object, bytes).unwrap();
            }
            1 => fs::write(&object, &bytes[..1]).unwrap(),
            _ => fs::remove_file(&object).unwrap(),
        }
        cached_success(&cached("run", &package, &cache).output().unwrap(), false);
        assert_eq!(bundle(&cache), object);
        cached_success(&cached("run", &package, &cache).output().unwrap(), true);
    }

    let valid_bundle = fs::read(&object).unwrap();
    fs::write(package.join("main.loom"), "fn main() { assert 42 }").unwrap();
    let invalid = cached("build", &package, &cache).output().unwrap();
    assert!(!invalid.status.success(), "{invalid:?}");
    let diagnostic = String::from_utf8_lossy(&invalid.stderr);
    assert!(!diagnostic.contains("loom cache:"), "{diagnostic}");
    assert!(!diagnostic.contains("loom-native phase:"), "{diagnostic}");
    assert_eq!(fs::read(&object).unwrap(), valid_bundle);

    fs::write(package.join("main.loom"), source).unwrap();
    let ir = directory.path().join("requested.ll");
    let emitted = cached("build", &package, &cache)
        .arg("--emit-ir")
        .arg(&ir)
        .output()
        .unwrap();
    success(&emitted);
    let trace = String::from_utf8_lossy(&emitted.stderr);
    assert!(!trace.contains("loom cache:"), "{trace}");
    assert!(trace.contains("loom-native phase: codegen"), "{trace}");
    let llvm = fs::read_to_string(ir).unwrap();
    assert!(
        llvm.lines()
            .any(|line| line.starts_with("define ") && line.contains("@main("))
    );
    assert_eq!(fs::read(&object).unwrap(), valid_bundle);

    for output in [
        cache.join("objects-v1/stage-0/bundle"),
        cache.join("objects-v1/stage-0/nested/bundle"),
        common::executable(&cache.join("objects-v1"), "future-output"),
    ] {
        let rejected = cached("build", &package, &cache)
            .arg("--output")
            .arg(&output)
            .output()
            .unwrap();
        assert!(!rejected.status.success(), "{rejected:?}");
        assert!(fs::symlink_metadata(output).is_err());
        assert_eq!(fs::read(&object).unwrap(), valid_bundle);
        no_staging_directories(&cache);
    }

    for command in ["check", "resolve", "emit-checked"] {
        let rejected = cached(command, &package, &cache).output().unwrap();
        assert!(!rejected.status.success(), "{command}: {rejected:?}");
        assert!(String::from_utf8_lossy(&rejected.stderr).contains("--object-cache"));
    }
}

#[test]
fn cache_hits_relink_current_runtime_while_libraries_remain_objects() {
    let directory = tempfile::tempdir().unwrap();
    let cache = directory.path().join("cache");
    let managed = package(directory.path(), "managed", &managed_source("runtime"));
    cached_success(&cached("run", &managed, &cache).output().unwrap(), false);

    let missing_runtime = directory.path().join("missing-runtime.a");
    let invalid_runtime = directory.path().join("invalid-runtime.a");
    fs::write(&invalid_runtime, "not a native archive").unwrap();
    for runtime in [&missing_runtime, &invalid_runtime] {
        let output = cached("build", &managed, &cache)
            .env("LOOM_RUNTIME_LIBRARY", runtime)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{output:?}");
        cache_trace(&output, true);
        no_staging_directories(&cache);
    }
    let output = cached("run", &managed, &cache).output().unwrap();
    cached_success(&output, true);
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "runtime 雪\n");

    let library = package(
        directory.path(),
        "library",
        "import std.text.concat\npub fn greeting(value Text) Text { concat(value, \" 雪\") }",
    );
    let first = directory.path().join("first-library.o");
    let second = directory.path().join("second-library.o");
    for (output, hit) in [(&first, false), (&second, true)] {
        cached_success(
            &cached("build", &library, &cache)
                .arg("--output")
                .arg(output)
                .env("LOOM_RUNTIME_LIBRARY", &missing_runtime)
                .env("LOOM_CC", directory.path().join("missing-linker"))
                .output()
                .unwrap(),
            hit,
        );
    }
    let bytes = fs::read(first).unwrap();
    assert!(!bytes.is_empty());
    assert_eq!(fs::read(second).unwrap(), bytes);
}

#[test]
fn native_identity_mismatch_preserves_existing_output_without_codegen_or_receipt() {
    let directory = tempfile::tempdir().unwrap();
    let package = package(directory.path(), "scalar", "fn main() { assert true }");
    let checked = common::command(&["emit-checked"])
        .arg(&package)
        .output()
        .unwrap();
    success(&checked);
    let input = directory.path().join("checked.lcir");
    fs::write(&input, checked.stdout).unwrap();
    let artifact = directory.path().join("existing.o");
    let sentinel = b"existing output must survive identity mismatch";
    fs::write(&artifact, sentinel).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_loom-native"))
        .arg(&input)
        .arg("--output")
        .arg(&artifact)
        .args(["--object-only", "--expect-cache-identity"])
        .arg("0".repeat(64))
        .env("LOOM_NATIVE_TIMINGS", "1")
        .env("LOOM_OPT_LEVEL", "2")
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("native cache identity changed or is unavailable"),
        "{stderr}"
    );
    assert!(!stderr.contains("loom-native phase: codegen"), "{stderr}");
    assert_eq!(fs::read(artifact).unwrap(), sentinel);
}
