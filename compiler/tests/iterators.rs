use std::process::Command;
mod common;
use common::success;

#[test]
fn source_iterators_keep_pull_order_sharing_and_fault_cleanup() {
    let package = "compiler/examples/iterators";
    for command in ["check", "test", "run"] {
        success(&common::loom(&[command, package]));
    }
    let temporary = tempfile::tempdir().unwrap();
    let executable = common::executable(temporary.path(), "iterators");
    let tests = common::executable(temporary.path(), "iterator-tests");
    for level in ["0", "2"] {
        success(
            &common::command(&["test", "compiler/std/iter", "--no-run"])
                .arg("--output")
                .arg(&tests)
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        success(
            &Command::new(&tests)
                .env("LOOM_GC_STRESS", "1")
                .output()
                .unwrap(),
        );
        success(
            &common::command(&["build", package])
                .arg("--output")
                .arg(&executable)
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        success(
            &Command::new(&executable)
                .env("LOOM_GC_STRESS", "1")
                .output()
                .unwrap(),
        );
    }
}

#[test]
fn scoped_file_lines_keep_boundaries_errors_and_cleanup() {
    let temporary = tempfile::tempdir().unwrap();
    let package = "compiler/examples/file_lines";
    for mode in ["check", "test"] {
        success(&common::loom(&[mode, package]));
    }
    let library = common::root().join("compiler/std/file/lines");
    let tests = common::executable(temporary.path(), "lines-tests");
    let executable = common::executable(temporary.path(), "lines-app");
    let input = temporary.path().join("input.txt");
    let invalid = temporary.path().join("invalid.txt");
    std::fs::write(&input, "first\r\n\n世界\nlast\r").unwrap();
    std::fs::write(&invalid, b"good\n\xff\n").unwrap();
    for level in ["0", "2"] {
        success(
            &common::command(&["test", library.to_str().unwrap(), "--no-run"])
                .arg("--output")
                .arg(&tests)
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        success(
            &Command::new(&tests)
                .current_dir(temporary.path())
                .env("LOOM_GC_STRESS", "1")
                .output()
                .unwrap(),
        );
        success(
            &common::command(&["build", package])
                .arg("--output")
                .arg(&executable)
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        let output = Command::new(&executable)
            .arg(&input)
            .env("LOOM_GC_STRESS", "1")
            .output()
            .unwrap();
        success(&output);
        assert_eq!(output.stdout, b"4\n");
        let output = Command::new(&executable)
            .arg(&invalid)
            .env("LOOM_GC_STRESS", "1")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(output.stderr, b"cannot read UTF-8 lines\n");
    }
    let output = common::command(&["run", package])
        .arg("--")
        .arg(&input)
        .output()
        .unwrap();
    success(&output);
    assert_eq!(output.stdout, b"4\n");
}

#[test]
fn scoped_factory_pipelines_stop_early_and_do_not_erase_errors() {
    let package = "compiler/examples/file_pipeline";
    for mode in ["check", "test"] {
        success(&common::loom(&[mode, package]));
    }
    let temporary = tempfile::tempdir().unwrap();
    let executable = common::executable(temporary.path(), "file-pipeline");
    let input = temporary.path().join("input.txt");
    for level in ["0", "2"] {
        success(
            &common::command(&["build", package])
                .arg("--output")
                .arg(&executable)
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        for (contents, expected) in [
            (
                &b"\nfirst\r\n\nsecond\n\xff\n"[..],
                Ok(&b"first\nsecond\n"[..]),
            ),
            (&b"\nfirst\n\xff\n"[..], Err(())),
        ] {
            std::fs::write(&input, contents).unwrap();
            let output = Command::new(&executable)
                .arg(&input)
                .env("LOOM_GC_STRESS", "1")
                .output()
                .unwrap();
            match expected {
                Ok(text) => {
                    success(&output);
                    assert_eq!(output.stdout, text);
                }
                Err(()) => {
                    assert_eq!(output.status.code(), Some(1));
                    assert_eq!(output.stderr, b"cannot read UTF-8 lines\n");
                }
            }
        }
    }
}
