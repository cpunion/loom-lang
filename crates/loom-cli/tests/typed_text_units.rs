use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

static TEST_RUNTIME: OnceLock<PathBuf> = OnceLock::new();

fn runtime_bundle_root() -> &'static PathBuf {
    TEST_RUNTIME.get_or_init(|| {
        std::env::var_os("LOOM_TEST_RUNTIME_BUNDLE")
            .or_else(|| std::env::var_os("LOOM_RUNTIME_BUNDLE"))
            .map_or_else(
                || {
                    panic!(
                        "native CLI tests require LOOM_TEST_RUNTIME_BUNDLE or \
                         LOOM_RUNTIME_BUNDLE; prepare one with `loomc runtime pack`"
                    )
                },
                PathBuf::from,
            )
    })
}

fn loomc() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_loomc"));
    command.env("LOOM_RUNTIME_BUNDLE", runtime_bundle_root());
    command
}

fn assert_success(operation: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{operation} failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn write_fixture(root: &Path) {
    std::fs::write(
        root.join("main.loom"),
        include_str!("../../../fixtures/lcir-text-from-utf8-units/main.loom"),
    )
    .expect("write Text.from_utf8_units fixture");
}

#[test]
fn text_from_utf8_units_closes_native_check_build_test_and_run() {
    let project = tempfile::tempdir().expect("create Text.from_utf8_units project");
    write_fixture(project.path());

    let check = loomc()
        .args(["--no-cache", "check"])
        .arg(project.path())
        .output()
        .expect("check Text.from_utf8_units source through the production CLI");
    assert_success("check", &check);

    let object_path = project.path().join("text-from-utf8-units.o");
    let build = loomc()
        .args(["--no-cache", "build", "--emit", "object", "--output"])
        .arg(&object_path)
        .arg(project.path())
        .output()
        .expect("build Text.from_utf8_units source through the production CLI");
    assert_success("build", &build);

    let object = std::fs::read(&object_path).expect("read Text.from_utf8_units object");
    for required in [
        b"loom.lcir.fn".as_slice(),
        b"loom_runtime_text_from_utf8_units_typed_v1",
        b"loom_gc_typed_root_push_v1",
        b"loom_gc_typed_root_pop_v1",
    ] {
        assert!(
            contains_bytes(&object, required),
            "Text.from_utf8_units object omitted `{}`",
            String::from_utf8_lossy(required),
        );
    }
    for forbidden in [
        b"loom.fn.".as_slice(),
        b"loom.Value",
        b"ValueNode",
        b"loom_gc_root_push_v1",
        b"loom_gc_root_pop_v1",
        b"loom_executor_",
    ] {
        assert!(
            !contains_bytes(&object, forbidden),
            "Text.from_utf8_units object exposed `{}`",
            String::from_utf8_lossy(forbidden),
        );
    }

    let tests = loomc()
        .args(["--no-cache", "test"])
        .arg(project.path())
        .output()
        .expect("test Text.from_utf8_units source through the production CLI");
    assert_success("test", &tests);
    assert!(
        String::from_utf8_lossy(&tests.stdout)
            .contains("passed lcir_text_from_utf8_units.typedTextFromUtf8Units"),
        "unexpected test output:\n{}",
        String::from_utf8_lossy(&tests.stdout),
    );

    let run = loomc()
        .args(["--no-cache", "run"])
        .arg(project.path())
        .output()
        .expect("run Text.from_utf8_units source through the production CLI");
    assert_success("run", &run);
    assert_eq!(run.stdout, b"Unit\n");
}
