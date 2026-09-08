use std::{fs, process::Command};
mod common;
use common::{loom, success};

#[test]
fn source_collections_keep_managed_values_and_private_storage() {
    let temporary = tempfile::tempdir().unwrap();
    let executable = common::executable(temporary.path(), "collections");
    success(&loom(&[
        "build",
        common::root()
            .join("compiler/examples/collections")
            .to_str()
            .unwrap(),
        "--output",
        executable.to_str().unwrap(),
    ]));
    success(
        &Command::new(executable)
            .env("LOOM_GC_STRESS", "1")
            .output()
            .unwrap(),
    );

    for (source, diagnostic) in [
        (
            "import std.map.new\nfn main() { discard new[Float, Int]() }",
            "required concept",
        ),
        (
            "import std.map.new\nfn main() { let map = new[Int, Int]()\ndiscard map.state.cells }",
            "cannot inspect a private type",
        ),
    ] {
        fs::write(temporary.path().join("main.loom"), source).unwrap();
        let output = loom(&["check", temporary.path().to_str().unwrap()]);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(diagnostic),
            "{output:?}"
        );
    }
}
