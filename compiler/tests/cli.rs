use std::{
    fs,
    process::{Command, Stdio},
};
mod common;
use common::{loom, success};

#[test]
fn automatic_execution_is_private_and_preserves_retained_outputs() {
    let package = tempfile::tempdir().unwrap();
    fs::write(
        package.path().join("main.loom"),
        r#"import std.build.option
import std.io.write_text
import std.process.exit_code

fn emit() {
    discard write_text(option("case", "default"))
}

fn main() {
    if option("case", "default") == "failure" {
        exit_code(7)
    }
    emit()
}

test fn emits() {
    if option("case", "default") == "failure" {
        assert false
    }
    emit()
}
"#,
    )
    .unwrap();
    let target = package.path().join("target");
    fs::create_dir(&target).unwrap();
    // Existing build outputs and abandoned/foreign entries are not ours.
    let main = common::executable(&target, "main");
    let tests = common::executable(&target, "tests");
    fs::write(&main, b"retained main").unwrap();
    fs::write(&tests, b"retained tests").unwrap();
    fs::create_dir(target.join("execute-0")).unwrap();
    fs::write(target.join("execute-0/keep"), b"keep").unwrap();
    fs::write(target.join("execute-1"), b"not a directory").unwrap();
    for mode in ["run", "test"] {
        let children: Vec<_> = [("first", "0"), ("second", "2")]
            .into_iter()
            .map(|(value, level)| {
                let child = common::command(&[mode])
                    .arg(package.path())
                    .args(["--build-option", &format!("case={value}")])
                    .env("LOOM_OPT_LEVEL", level)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap();
                (value, child)
            })
            .collect();
        for (value, child) in children {
            let output = child.wait_with_output().unwrap();
            success(&output);
            let expected = if mode == "test" {
                format!("{value}1 tests passed\n")
            } else {
                value.to_owned()
            };
            assert_eq!(output.stdout, expected.as_bytes());
        }
    }
    for (mode, code) in [("run", 7), ("test", 1)] {
        let failed = common::command(&[mode])
            .arg(package.path())
            .args(["--build-option", "case=failure"])
            .output()
            .unwrap();
        assert_eq!(failed.status.code(), Some(code));
    }
    let failed = common::command(&["run"])
        .arg(package.path())
        .env("LOOM_OPT_LEVEL", "invalid")
        .output()
        .unwrap();
    assert!(!failed.status.success());
    assert_eq!(fs::read(&main).unwrap(), b"retained main");
    assert_eq!(fs::read(&tests).unwrap(), b"retained tests");
    assert_eq!(fs::read(target.join("execute-0/keep")).unwrap(), b"keep");
    assert_eq!(
        fs::read(target.join("execute-1")).unwrap(),
        b"not a directory"
    );
    assert_eq!(fs::read_dir(&target).unwrap().count(), 4);
}

#[test]
fn test_no_run_builds_a_real_test_binary_without_executing_tests() {
    let package = tempfile::tempdir().unwrap();
    fs::write(
        package.path().join("main.loom"),
        "fn private_value() Int { 42 }\ntest fn deliberately_fails() { assert private_value() == 0 }",
    )
    .unwrap();
    let executable = common::executable(package.path(), "compiled-tests");
    let output = loom(&[
        "test",
        package.path().to_str().unwrap(),
        "--no-run",
        "--output",
        executable.to_str().unwrap(),
    ]);
    success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("compiled "));
    assert!(String::from_utf8_lossy(&output.stdout).contains(executable.to_str().unwrap()));
    assert!(executable.is_file());
    assert!(!Command::new(&executable).output().unwrap().status.success());
    assert!(
        !loom(&["test", package.path().to_str().unwrap()])
            .status
            .success()
    );

    // No tests is not a request to produce a runnable main or a stale artifact.
    fs::write(
        package.path().join("main.loom"),
        "fn main() { assert false }",
    )
    .unwrap();
    let absent = common::executable(package.path(), "no-tests");
    let output = loom(&[
        "test",
        package.path().to_str().unwrap(),
        "--no-run",
        "--output",
        absent.to_str().unwrap(),
    ]);
    success(&output);
    assert_eq!(output.stdout, b"0 tests\n");
    assert!(!absent.exists());
}

#[test]
fn test_filters_select_native_roots_without_skipping_checks_or_reusing_other_filters() {
    let package = tempfile::tempdir().unwrap();
    fs::write(
        package.path().join("loom.toml"),
        "[module]\nname='filtertrial'\n",
    )
    .unwrap();
    let path = package.path().to_str().unwrap();
    let source = package.path().join("main.loom");
    let original = r#"
import std.io.write_text

fn private_answer() Int {
    42
}

async fn answer() Int {
    private_answer()
}

test fn chosen_sync() {
    assert private_answer() == 42
}

test async fn chosen_async() {
    assert answer().await == 42
}

test fn excluded_failure() {
    discard write_text("unselected-test-ran\n")
    assert false
}
"#;
    fs::write(&source, original).unwrap();
    let cache = package.path().join("cache");
    let filtered = |filter: &str| {
        common::command(&["test", path, "--filter", filter])
            .arg("--frontend-cache")
            .arg(&cache)
            .env("LOOM_NATIVE_TIMINGS", "1")
            .output()
            .unwrap()
    };
    let selected = filtered("chosen");
    success(&selected);
    assert_eq!(selected.stdout, b"2 tests passed\n");
    assert!(String::from_utf8_lossy(&selected.stderr).contains("frontend miss"));
    let hit = filtered("chosen");
    success(&hit);
    assert!(String::from_utf8_lossy(&hit.stderr).contains("frontend hit"));
    let exact = filtered("filtertrial.chosen_sync");
    success(&exact);
    assert_eq!(exact.stdout, b"1 tests passed\n");
    let trace = String::from_utf8_lossy(&exact.stderr);
    assert!(
        trace.contains("frontend miss")
            && trace.contains("bodies reused ")
            && !trace.contains("bodies reused 0")
    );
    let absent = filtered("Chosen");
    success(&absent);
    assert_eq!(absent.stdout, b"0 tests matched Chosen\n");
    let literal = filtered("chosen.*");
    success(&literal);
    assert_eq!(literal.stdout, b"0 tests matched chosen.*\n");
    let failed = filtered("excluded_failure");
    assert_eq!(failed.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&failed.stdout).contains("unselected-test-ran"));
    let executable = common::executable(package.path(), "chosen-tests");
    let ir = package.path().join("chosen.ll");
    for level in ["0", "2"] {
        success(
            &common::command(&["test", path, "--filter", "chosen", "--no-run"])
                .arg("--output")
                .arg(&executable)
                .arg("--emit-ir")
                .arg(&ir)
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        success(&common::run_tasks(&executable));
        assert!(
            !fs::read_to_string(&ir)
                .unwrap()
                .contains("unselected-test-ran")
        );
    }
    fs::write(
        &source,
        format!("{original}\nfn invalid() Int ensures result > 0 {{ 0 }}\n"),
    )
    .unwrap();
    for filter in ["chosen", "no_such_test"] {
        let invalid = filtered(filter);
        assert!(!invalid.status.success());
        assert!(String::from_utf8_lossy(&invalid.stderr).contains("postcondition"));
    }
}

#[test]
fn recursive_tests_keep_package_scopes_module_boundaries_and_failure_status() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let write = |name: &str, source: &str| {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    };
    write(
        "loom.toml",
        "[module]\nname='suite'\n[dependencies.other]\npath='nested'\n",
    );
    write(
        "main.loom",
        r#"import suite.alpha.answer
import other.value

fn private_value() Int {
    answer() + value()
}

test fn embedded() {
    assert private_value() == 42
}
"#,
    );
    write(
        "same_package_test.loom",
        "test fn private_access() {\n    assert private_value() == 42\n}\n",
    );
    write("alpha/main.loom", "pub fn answer() Int {\n    40\n}\n");
    write(
        "alpha/failure_test.loom",
        "test fn failed() {\n    assert answer() == 0\n}\n",
    );
    write("nested/loom.toml", "[module]\nname='other'\n");
    write(
        "nested/main.loom",
        "pub fn value() Int {\n    2\n}\ntest fn excluded() {\n    assert false\n}\n",
    );
    write("nested/invalid_test.loom", "not valid Loom syntax");
    write(
        "group/last/only_test.loom",
        r#"import std.io.write_text

test fn still_runs() {
    discard write_text("last-package-ran\n")
}
"#,
    );
    write("library/value.loom", "pub fn answer() Int {\n    42\n}\n");
    for skipped in [".hidden", "target", "node_modules"] {
        write(&format!("{skipped}/invalid.loom"), "not valid Loom syntax");
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join("nested"), root.join("linked")).unwrap();

    let path = root.to_str().unwrap();
    let ordinary = loom(&["test", path]);
    success(&ordinary);
    assert_eq!(ordinary.stdout, b"2 tests passed\n");

    let filtered = loom(&["test", path, "--recursive", "--filter", "embedded"]);
    success(&filtered);
    let text = String::from_utf8_lossy(&filtered.stdout);
    assert!(text.contains(": 1 tests passed\n"));
    assert!(text.ends_with("4 packages tested\n"));
    assert!(!text.contains("last-package-ran"));

    let compiled = loom(&["test", path, "--recursive", "--no-run"]);
    success(&compiled);
    let text = String::from_utf8_lossy(&compiled.stdout);
    assert!(text.ends_with("4 packages checked\n"), "{compiled:?}");
    assert!(!text.contains("last-package-ran"));
    for package in ["", "alpha", "group/last"] {
        assert!(common::executable(&root.join(package).join("target"), "tests").is_file());
    }
    assert!(!root.join("library/target").exists());
    assert!(!root.join("nested/target").exists());

    let failed = loom(&["test", path, "--recursive"]);
    assert_eq!(failed.status.code(), Some(1), "{failed:?}");
    let text = String::from_utf8_lossy(&failed.stdout);
    assert!(text.contains(": 2 tests passed\n"), "{failed:?}");
    assert!(text.contains("last-package-ran\n"), "{failed:?}");
    assert!(text.contains(": 0 tests\n"), "{failed:?}");
    assert!(
        String::from_utf8_lossy(&failed.stderr).contains("1 of 4 packages failed"),
        "{failed:?}"
    );

    write(
        "alpha/failure_test.loom",
        "test fn fixed() {\n    assert answer() == 40\n}\n",
    );
    let passed = loom(&["test", path, "--recursive"]);
    success(&passed);
    assert!(passed.stdout.ends_with(b"4 packages tested\n"));

    let empty = root.join("empty");
    fs::create_dir(&empty).unwrap();
    let absent = loom(&["test", empty.to_str().unwrap(), "--recursive"]);
    assert_eq!(absent.status.code(), Some(1), "{absent:?}");
    assert!(String::from_utf8_lossy(&absent.stderr).contains("no Loom packages under"));
}
