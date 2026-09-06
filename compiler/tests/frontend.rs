//! Exercise the bootstrapped Loom compiler, not a host-language frontend.

use std::{
    fs,
    path::Path,
    process::{Command, Output},
};
mod common;
use common::{loom, success};

fn source_files(directory: &Path, paths: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            source_files(&path, paths);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "loom")
        {
            paths.push(path);
        }
    }
}

fn source_compiler(compiler: &Path, args: &[&str]) -> Output {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    Command::new(compiler)
        .args(args)
        .args(["--std", root.join("compiler/std").to_str().unwrap()])
        .args(["--native-tool", env!("CARGO_BIN_EXE_loom-native")])
        .current_dir(root)
        .output()
        .unwrap()
}

#[test]
fn checked_export_needs_no_native_tool_and_retains_required_proofs() {
    let temporary = tempfile::tempdir().unwrap();
    let package = temporary.path().join("package");
    fs::create_dir(&package).unwrap();
    let source = package.join("main.loom");
    fs::write(
        &source,
        "fn answer() Int ensures result == 42 { 42 }\nfn main() { assert answer() == 42 }",
    )
    .unwrap();
    let export = || {
        Command::new(common::compiler())
            .arg("emit-checked")
            .arg(&package)
            .arg("--std")
            .arg(common::root().join("compiler/std"))
            .args(["--native-tool", "missing-native-tool"])
            .output()
            .unwrap()
    };
    let output = export();
    success(&output);
    assert!(output.stdout.starts_with(b"loom-checked-1\n"));
    assert!(output.stderr.is_empty());
    let checked = temporary.path().join("program.checked");
    fs::write(&checked, output.stdout).unwrap();
    let executable = common::executable(temporary.path(), "exported");
    success(
        &Command::new(env!("CARGO_BIN_EXE_loom-native"))
            .arg(&checked)
            .arg("--output")
            .arg(&executable)
            .output()
            .unwrap(),
    );
    success(&Command::new(executable).output().unwrap());

    fs::write(&source, "fn answer() Int ensures result == 42 { 0 }").unwrap();
    let rejected = export();
    assert_eq!(rejected.status.code(), Some(1));
    assert!(rejected.stdout.is_empty());
    assert!(!rejected.stderr.is_empty());
}

#[test]
fn loom_compiler_checks_its_packages_and_reports_real_diagnostics() {
    let compiler = Path::new(env!("CARGO_MANIFEST_DIR"));
    let frontend = compiler.join("loom");
    let temp = tempfile::tempdir().unwrap();
    let artifact = common::compiler();
    success(&loom(&["check", frontend.to_str().unwrap()]));

    // Exercise real command-line input and a corpus, without an all-program GC matrix.
    let mut sources = Vec::new();
    source_files(&frontend, &mut sources);
    source_files(&compiler.join("std"), &mut sources);
    source_files(&compiler.join("examples"), &mut sources);
    sources.sort();
    for mode in ["lex", "parse"] {
        let output = Command::new(&artifact)
            .arg(mode)
            .args(&sources)
            .output()
            .unwrap();
        success(&output);
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).lines().count(),
            sources.len()
        );
        assert!(output.stderr.is_empty());
    }

    // scripts/bootstrap.sh already builds and compares stage 2/3 once.
    let stage2 = common::executable(&common::root().join("target"), "loom-stage2");
    let stage3 = &artifact;
    for package in [
        "std/loom/source",
        "std/loom/lexer",
        "std/loom/parser",
        "std/loom/binding",
        "std/loom/manifest",
        "std/loom/project",
        "std/loom/proof",
        "std/loom/checking",
        "std/loom/analysis",
        "std/loom/eval",
        "loom/artifact",
        "std/int",
        "std/float",
        "std/display",
        "std/text",
        "std/bytes",
        "std/list",
        "std/option",
        "std/result",
        "std/unicode",
        "std/fs",
        "std/process",
        "examples/scalar",
        "examples/data",
        "examples/syntax",
        "examples/project",
        "examples/semantic",
        "examples/comptime",
        "examples/arguments",
        "examples/tuples",
        "examples/floats",
        "examples/concepts",
    ] {
        let output = source_compiler(stage3, &["test", compiler.join(package).to_str().unwrap()]);
        assert!(output.status.success(), "{package}: {output:?}");
    }
    success(&source_compiler(
        stage3,
        &["run", compiler.join("examples/data").to_str().unwrap()],
    ));

    // Ordinary in-memory syntax/semantic clients need no compiler module or I/O
    // discovery. These examples deliberately print their results.
    for example in ["syntax", "semantic"] {
        let package = compiler.join("examples").join(example);
        let binary = common::executable(temp.path(), example);
        let ir = temp.path().join(format!("{example}.ll"));
        success(&source_compiler(
            stage3,
            &[
                "build",
                package.to_str().unwrap(),
                "--output",
                binary.to_str().unwrap(),
                "--emit-ir",
                ir.to_str().unwrap(),
            ],
        ));
        success(&Command::new(binary).output().unwrap());
        let ir = fs::read_to_string(ir).unwrap();
        for absent in [
            "loom_rt_process_arg",
            "loom_rt_process_run",
            "loom_rt_directory_",
            "loom_rt_path_",
            "loom_rt_file_open",
            "loom_rt_file_read",
            "loom_rt_file_create",
        ] {
            assert!(!ir.contains(absent), "{example} API leaked {absent}");
        }
    }

    let arguments = common::executable(temp.path(), "arguments");
    success(&source_compiler(
        stage3,
        &[
            "build",
            compiler.join("examples/arguments").to_str().unwrap(),
            "--output",
            arguments.to_str().unwrap(),
        ],
    ));
    let output = Command::new(&arguments).arg("+0010").output().unwrap();
    success(&output);
    assert_eq!(output.stdout, b"55\n");
    for (input, error) in [
        ("bad", "expected a decimal integer\n"),
        ("10001", "count must be between 0 and 10000\n"),
    ] {
        let output = Command::new(&arguments).arg(input).output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(output.stderr, error.as_bytes());
    }

    // Source checks do not need a native tool. Compare selected type/proof
    // failures across stages, without maintaining a dual-backend test matrix.
    let rejected = temp.path().join("rejected");
    fs::create_dir(&rejected).unwrap();
    for source in [
        "fn f() Int { true }",
        "fn f(x Int) Int ensures result > x { x }",
    ] {
        fs::write(rejected.join("main.loom"), source).unwrap();
        let first = source_compiler(&stage2, &["check", rejected.to_str().unwrap()]);
        let second = source_compiler(stage3, &["check", rejected.to_str().unwrap()]);
        assert_eq!(first.status.code(), Some(1));
        assert_eq!(first.status.code(), second.status.code());
        assert!(!first.stderr.is_empty());
        assert_eq!(first.stderr, second.stderr);
    }

    let good = temp.path().join("订单.loom");
    fs::write(&good, "fn main() { let 订单 = \"hé\\n\" }\r\n").unwrap();
    let bad = temp.path().join("invalid.loom");
    fs::write(&bad, "// comment\r\nlet 订单: Int\n").unwrap();
    let output = Command::new(&artifact)
        .arg("lex")
        .args([&good, &bad])
        .env("LOOM_GC_STRESS", "1")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stdout).contains("订单.loom:"));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("invalid.loom:2:7: write a type after its name without a colon")
    );
    fs::write(&bad, "// comment\r\nfn f() Unit {}\n").unwrap();
    let output = Command::new(&artifact)
        .arg("parse")
        .args([&good, &bad])
        .env("LOOM_GC_STRESS", "1")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stdout).contains("订单.loom: 1 declarations"));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("invalid.loom:2:8: omit the Unit return annotation")
    );
    assert_eq!(
        Command::new(&artifact).output().unwrap().status.code(),
        Some(2)
    );

    let invalid_slice = temp.path().join("slice");
    fs::create_dir(&invalid_slice).unwrap();
    fs::write(
        invalid_slice.join("main.loom"),
        "import std.text.slice\nfn main() { discard slice(\"é\", 0, 1) }",
    )
    .unwrap();
    let output = loom(&["run", invalid_slice.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("UTF-8 boundary"));
}

#[test]
fn ordinary_project_tool_selects_packages_and_tests_without_a_compiler_child() {
    let temp = tempfile::tempdir().unwrap();
    let root = common::root();
    let tool = common::executable(temp.path(), "project");
    let ir = temp.path().join("project.ll");
    success(&loom(&[
        "build",
        root.join("compiler/examples/project").to_str().unwrap(),
        "--output",
        tool.to_str().unwrap(),
        "--emit-ir",
        ir.to_str().unwrap(),
    ]));
    let ir = fs::read_to_string(ir).unwrap();
    for absent in ["loom_rt_process_run", "loom_rt_file_create"] {
        assert!(!ir.contains(absent), "project API leaked {absent}");
    }

    let package = temp.path().join("app");
    for directory in ["dep", "testutil", "unselected"] {
        fs::create_dir_all(package.join(directory)).unwrap();
    }
    fs::write(
        package.join("loom.toml"),
        "schema = 2\nlanguage = \"0.4\"\n[module]\nname = \"app\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let production = "import app.dep.open\nfn secret() Int { 7 }\npub fn answer() Int { open() }\ntest fn inline() { assert secret() == helper() }\n";
    fs::write(package.join("main.loom"), production).unwrap();
    fs::write(
        package.join("main_test.loom"),
        "import app.testutil.extra\nfn helper() Int { extra() }\ntest fn colocated() { assert secret() == helper() }\n",
    )
    .unwrap();
    fs::write(
        package.join("dep/main.loom"),
        "pub fn open() Int { 42 }\nfn hidden() {}\ntest fn dependency() { absent() }\n",
    )
    .unwrap();
    fs::write(package.join("dep/broken_test.loom"), "invalid source").unwrap();
    fs::write(
        package.join("testutil/main.loom"),
        "import app.answer\npub fn extra() Int { assert answer() == 42\n7 }",
    )
    .unwrap();
    fs::write(package.join("unselected/main.loom"), "invalid source").unwrap();
    let inspect = |tests| {
        let mut command = Command::new(&tool);
        command.arg(&package).arg(root.join("compiler/std"));
        if tests {
            command.arg("--tests");
        }
        command.output().unwrap()
    };
    let output = inspect(false);
    success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("package app\nselected files 2\n"));
    assert!(stdout.contains("fn app.secret @"));
    assert!(stdout.contains("pub fn app.answer @"));
    assert!(!stdout.contains("test fn"));
    assert!(!stdout.contains("app.helper"));
    let output = inspect(true);
    success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("package app\nselected files 4\n"));
    assert!(stdout.contains("test fn app.inline @"));
    assert!(stdout.contains("test-only fn app.helper @"));
    assert!(stdout.contains("test fn app.colocated @"));
    assert!(!stdout.contains("app.dep.dependency"));

    // The same selection also type-checks and runs through the compiler.
    success(&loom(&["check", package.to_str().unwrap()]));
    success(&loom(&["test", package.to_str().unwrap()]));
    // Test helpers may depend on the root's production API; production cycles
    // remain invalid, including when tests are selected.
    fs::write(
        package.join("dep/main.loom"),
        "import app.answer\npub fn open() Int { answer() }",
    )
    .unwrap();
    let output = inspect(true);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("package import cycle"));
    fs::write(
        package.join("dep/main.loom"),
        "pub fn open() Int { 42 }\nfn hidden() {}",
    )
    .unwrap();
    fs::write(package.join("main.loom"), "import app.dep.hidden\n").unwrap();
    let output = inspect(false);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("main.loom:1:1: import does not name an accessible declaration")
    );

    #[cfg(unix)]
    {
        // A directory alias must not give a foreign module a second identity.
        let foreign = temp.path().join("foreign");
        fs::create_dir_all(foreign.join("lib")).unwrap();
        fs::write(
            foreign.join("loom.toml"),
            "schema = 2\nlanguage = \"0.4\"\n[module]\nname = \"foreign\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(foreign.join("lib/main.loom"), "pub fn open() {}").unwrap();
        std::os::unix::fs::symlink(foreign.join("lib"), package.join("alias")).unwrap();
        fs::write(package.join("main.loom"), "import app.alias.open\n").unwrap();
        let output = inspect(false);
        assert_eq!(output.status.code(), Some(1));
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("import directory changes package identity through a symlink: app.alias")
        );
    }
}
