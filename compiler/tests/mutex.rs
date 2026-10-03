use std::fs;
mod common;
use common::success;

#[test]
fn source_mutex_guards_release_at_lexical_and_fault_boundaries() {
    let temporary = tempfile::tempdir().unwrap();
    let executable = common::executable(temporary.path(), "mutex");
    for level in ["0", "2"] {
        success(
            &common::command(&["test", "compiler/std/sync/mutex"])
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        success(&common::run_tasks(&common::executable(
            &common::root().join("compiler/std/sync/mutex/target"),
            "tests",
        )));
        success(
            &common::command(&[
                "build",
                "compiler/examples/mutex",
                "--output",
                executable.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        let output = common::run_tasks(&executable);
        success(&output);
        assert_eq!(output.stdout, b"scoped mutex released\n");
    }
}

#[test]
fn mutex_guards_cannot_be_dropped_copied_reconstructed_or_suspended() {
    let package = tempfile::tempdir().unwrap();
    for body in [
        "discard lock(mutex)",
        "let guard = lock(mutex)",
        "scoped guard = lock(mutex)\nscoped copied = guard",
        "scoped guard = lock(mutex)\nscoped copied = Guard { held = guard.held }",
        "scoped guard = lock(mutex)\nsleep_ns(0).await",
    ] {
        fs::write(package.path().join("main.loom"), format!(
            "import std.sync.mutex.new\nimport std.sync.mutex.lock\nimport std.sync.mutex.Guard\nimport std.time.sleep_ns\nasync fn main() {{\nlet mutex = new()\n{body}\n}}\n"
        )).unwrap();
        let output = common::loom(&["check", package.path().to_str().unwrap()]);
        assert!(
            !output.status.success(),
            "guard escape was accepted: {body}"
        );
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        assert!(
            !diagnostic.contains("unknown private runtime intrinsic"),
            "{diagnostic}"
        );
        assert!(
            diagnostic.contains("scoped")
                || diagnostic.contains("NoSuspend")
                || diagnostic.contains("fresh transferred payload"),
            "{diagnostic}"
        );
    }
}
