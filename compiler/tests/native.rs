use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn loom(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_loom"))
        .args(args)
        .output()
        .unwrap()
}

fn success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn source(text: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("main.loom"), text).unwrap();
    dir
}

fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
fn native_cli_closure_and_source_library() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/scalar");
    let directory = tempfile::tempdir().unwrap();
    let artifact = directory.path().join("app");
    let ir = directory.path().join("app.ll");
    success(&loom(&["check", path(&fixture)]));
    success(&loom(&[
        "build",
        path(&fixture),
        "--output",
        path(&artifact),
        "--emit-ir",
        path(&ir),
    ]));
    success(&Command::new(&artifact).output().unwrap());
    success(&loom(&["run", path(&fixture)]));
    let tests = loom(&["test", path(&fixture)]);
    success(&tests);
    assert_eq!(
        String::from_utf8_lossy(&tests.stdout).trim(),
        "2 tests passed"
    );
    let llvm = fs::read_to_string(ir).unwrap();
    assert!(
        llvm.lines()
            .any(|line| line.starts_with("define ") && line.contains("i32 @main("))
    );
    for forbidden in ["executor", "loom_runtime", "malloc", "universal"] {
        assert!(
            !llvm.contains(forbidden),
            "unexpected scalar IR dependency: {forbidden}"
        );
    }
    success(&loom(&[
        "test",
        path(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("std/int")),
    ]));
}

#[test]
fn production_library_excludes_both_test_forms() {
    let dir = source("pub fn answer() Int { 42 }\ntest fn ignored() { absent() }\n");
    // A test file is not even parsed during a production build.
    fs::write(dir.path().join("broken_test.loom"), "not valid source").unwrap();
    let object = dir.path().join("library.o");
    let ir = dir.path().join("library.ll");
    success(&loom(&[
        "build",
        path(dir.path()),
        "--output",
        path(&object),
        "--emit-ir",
        path(&ir),
    ]));
    assert!(object.metadata().unwrap().len() > 0);
    let llvm = fs::read_to_string(ir).unwrap();
    assert!(!llvm.contains("@main("));
    assert!(!llvm.contains("absent"));
    assert_eq!(
        llvm.lines()
            .filter(|line| line.starts_with("define "))
            .count(),
        1
    );
    assert!(!loom(&["test", path(dir.path())]).status.success());
}

#[test]
fn rejects_unsound_proofs_types_and_ignored_values() {
    for text in [
        "fn f(x Int) Int ensures result > x { x }",
        "fn f(x Int) Int ensures result >= 0 { var y = x\nwhile y < 0 { y = y + 1 }\ny }",
        "fn main() { let x = true + 1\ndiscard x }",
        "fn main() { 42 }",
        "fn main() Unit {}",
        "pub test fn exposed() {}",
        "fn main() { Unit }",
        "fn main() { await f() }",
    ] {
        let dir = source(text);
        let output = loom(&["check", path(dir.path())]);
        assert!(!output.status.success(), "unexpectedly accepted {text}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("main.loom:"));
    }
}

#[test]
fn native_faults_are_checked_not_llvm_undefined_behavior() {
    for (body, message) in [
        ("discard 9223372036854775807 + 1", "overflow"),
        ("let n = -9223372036854775808\ndiscard -n", "overflow"),
        ("discard 1 / 0", "zero"),
        ("discard -9223372036854775808 / -1", "overflow"),
        ("discard -9223372036854775808 % -1", "overflow"),
    ] {
        let dir = source(&format!("fn main() {{ {body} }}"));
        let output = loom(&["run", path(dir.path())]);
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(message), "expected {message}: {stderr}");
    }
    let dir =
        source("fn f(x Int) Int requires x > 0 { assert false\nx }\nfn main() { discard f(0) }");
    let output = loom(&["run", path(dir.path())]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("precondition failed"));
}

#[test]
fn control_flow_short_circuits_and_preserves_returns() {
    let dir = source(
        r#"
fn fail() Bool { assert false
    true }
fn choose(x Int) Int {
    if x > 0 { return 7 }
    8
}
fn both(x Bool) Int {
    if x { return 3 } else { return 4 }
}
fn main() {
    assert !(false && fail())
    assert true || fail()
    assert choose(1) == 7
    assert choose(0) == 8
    assert both(true) == 3
    assert both(false) == 4
    var n = 0
    while n < 3 { n = n + 1 }
    assert n == 3
}
"#,
    );
    success(&loom(&["run", path(dir.path())]));
}

#[test]
fn test_helpers_do_not_leak_into_production_binding() {
    let dir =
        source("fn production() Int { helper() }\ntest fn example() { discard production() }");
    fs::write(dir.path().join("main_test.loom"), "fn helper() Int { 1 }").unwrap();
    assert!(!loom(&["test", path(dir.path())]).status.success());
}

#[cfg(unix)]
#[test]
fn output_aliases_cannot_overwrite_inputs_or_each_other() {
    use std::os::unix::fs::symlink;
    let dir = source("pub fn value() Int { 42 }");
    let manifest = "[module]\nname = 'demo'\nversion = '0.1.0'\n";
    fs::write(dir.path().join("loom.toml"), manifest).unwrap();
    let linked_manifest = dir.path().join("manifest.ll");
    symlink(dir.path().join("loom.toml"), &linked_manifest).unwrap();
    assert!(
        !loom(&[
            "build",
            path(dir.path()),
            "--emit-ir",
            path(&linked_manifest)
        ])
        .status
        .success()
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("loom.toml")).unwrap(),
        manifest
    );

    let real = dir.path().join("real");
    let alias = dir.path().join("alias");
    fs::create_dir(&real).unwrap();
    symlink(&real, &alias).unwrap();
    let output = loom(&[
        "build",
        path(dir.path()),
        "--output",
        path(&real.join("artifact")),
        "--emit-ir",
        path(&alias.join("artifact")),
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("paths must differ"));

    // Publication replaces the requested directory entry, not another hard link.
    let hard_link = dir.path().join("artifact");
    fs::hard_link(dir.path().join("main.loom"), &hard_link).unwrap();
    success(&loom(&[
        "build",
        path(dir.path()),
        "--output",
        path(&hard_link),
    ]));
    assert_eq!(
        fs::read_to_string(dir.path().join("main.loom")).unwrap(),
        "pub fn value() Int { 42 }"
    );
}
