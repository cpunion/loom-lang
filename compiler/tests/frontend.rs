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
    let stage2 = common::root().join("target/loom-stage2");
    let stage3 = &artifact;
    for package in [
        "std/loom/source",
        "std/loom/lexer",
        "std/loom/parser",
        "loom/binding",
        "loom/manifest",
        "loom/loading",
        "loom/proof",
        "loom/checking",
        "loom/artifact",
        "std/int",
        "std/text",
        "std/bytes",
        "std/list",
        "std/result",
        "std/unicode",
        "std/fs",
        "std/process",
        "examples/scalar",
        "examples/data",
        "examples/syntax",
    ] {
        success(&source_compiler(
            stage3,
            &["test", compiler.join(package).to_str().unwrap()],
        ));
    }
    success(&source_compiler(
        stage3,
        &["run", compiler.join("examples/data").to_str().unwrap()],
    ));

    // An ordinary user package imports syntax APIs without the compiler module.
    let syntax = compiler.join("examples/syntax");
    let syntax_binary = temp.path().join("syntax");
    let syntax_ir = temp.path().join("syntax.ll");
    success(&source_compiler(
        stage3,
        &[
            "build",
            syntax.to_str().unwrap(),
            "--output",
            syntax_binary.to_str().unwrap(),
            "--emit-ir",
            syntax_ir.to_str().unwrap(),
        ],
    ));
    success(&Command::new(syntax_binary).output().unwrap());
    let ir = fs::read_to_string(syntax_ir).unwrap();
    // The example deliberately prints its result; syntax itself needs neither
    // filesystem discovery/input nor compiler/process invocation.
    for absent in [
        "loom_rt_process_arg",
        "loom_rt_process_run",
        "loom_rt_directory_",
        "loom_rt_path_",
        "loom_rt_file_open",
        "loom_rt_file_read",
        "loom_rt_file_create",
    ] {
        assert!(!ir.contains(absent), "syntax API leaked {absent}");
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
