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
                         LOOM_RUNTIME_BUNDLE; prepare one with `loom runtime pack`"
                    )
                },
                PathBuf::from,
            )
    })
}

fn loom() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_loom"));
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
        include_str!("../../../fixtures/lcir-managed-bytes/main.loom"),
    )
    .expect("write managed-Bytes fixture");
    std::fs::write(
        root.join("main_test.loom"),
        include_str!("../../../fixtures/lcir-managed-bytes/main_test.loom"),
    )
    .expect("write managed-Bytes tests");
}

#[test]
fn managed_bytes_close_typed_native_check_build_test_and_run() {
    let project = tempfile::tempdir().expect("create managed-Bytes project");
    write_fixture(project.path());

    let check = loom()
        .args(["--no-cache", "check"])
        .arg(project.path())
        .output()
        .expect("check managed-Bytes source through the production CLI");
    assert_success("check", &check);

    let object_path = project.path().join("managed-bytes.o");
    let build = loom()
        .args(["--no-cache", "build", "--emit", "object", "--output"])
        .arg(&object_path)
        .arg(project.path())
        .output()
        .expect("build managed-Bytes source through the production CLI");
    assert_success("build", &build);

    let object = std::fs::read(&object_path).expect("read managed-Bytes object");
    for required in [
        b"loom.lcir.fn".as_slice(),
        b"loom_runtime_bytes_append_typed_v1",
        b"loom_runtime_bytes_decode_utf8_typed_v1",
        b"loom_gc_typed_root_push_v1",
        b"loom_gc_typed_root_pop_v1",
    ] {
        assert!(
            contains_bytes(&object, required),
            "managed-Bytes object omitted `{}`",
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
            "managed-Bytes object exposed `{}`",
            String::from_utf8_lossy(forbidden),
        );
    }

    let tests = loom()
        .args(["--no-cache", "test"])
        .arg(project.path())
        .output()
        .expect("test managed-Bytes source through the production CLI");
    assert_success("test", &tests);
    assert!(
        String::from_utf8_lossy(&tests.stdout).contains("passed standalone.typedBytes"),
        "unexpected test output:\n{}",
        String::from_utf8_lossy(&tests.stdout),
    );

    let run = loom()
        .args(["--no-cache", "run"])
        .arg(project.path())
        .output()
        .expect("run managed-Bytes source through the production CLI");
    assert_success("run", &run);
    assert_eq!(run.stdout, b"Unit\n");
}
