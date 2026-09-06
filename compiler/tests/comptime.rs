use std::{fs, process::Command};
mod common;
use common::{loom, success};

#[test]
fn compile_time_work_runs_natively_and_does_not_enter_runtime_reachability() {
    let example = common::root().join("compiler/examples/comptime");
    success(&loom(&["check", example.to_str().unwrap()]));
    success(&loom(&["run", example.to_str().unwrap()]));
    success(&loom(&["test", example.to_str().unwrap()]));
    success(
        &Command::new(example.join("target/tests"))
            .env("LOOM_GC_STRESS", "1")
            .output()
            .unwrap(),
    );

    let source = tempfile::tempdir().unwrap();
    fs::write(source.path().join("main.loom"),
        "fn fib(n Int) Int { if n < 2 { n } else { fib(n-1) + fib(n-2) } }\npub fn answer() Int ensures result == 55 { comptime { fib(10) } }\nfn main() { assert answer() == 55 }").unwrap();
    let artifact = source.path().join("app");
    let ir = source.path().join("app.ll");
    // Inspect unoptimized IR: compile-time-only functions are absent before LLVM DCE.
    success(
        &common::command(&[
            "build",
            source.path().to_str().unwrap(),
            "--output",
            artifact.to_str().unwrap(),
            "--emit-ir",
            ir.to_str().unwrap(),
        ])
        .env("LOOM_OPT_LEVEL", "0")
        .output()
        .unwrap(),
    );
    success(&Command::new(artifact).output().unwrap());
    let ir = fs::read_to_string(ir).unwrap();
    assert_eq!(
        ir.lines()
            .filter(|line| line.starts_with("define ") && line.contains("@loom.fn."))
            .count(),
        2
    );
    assert!(ir.contains("ret i64 55"));
    assert!(!ir.contains("loom_rt_"));
}

#[test]
fn compile_time_faults_captures_and_unproved_contracts_fail_closed() {
    let source = tempfile::tempdir().unwrap();
    for text in [
        "fn main() { discard comptime { 9223372036854775807 + 1 } }",
        "fn main() { discard comptime { -9223372036854775808 / -1 } }",
        "fn main() { discard comptime { 1 / 0 } }",
        "fn main() { let x = 1\ndiscard comptime { x } }",
        "fn main() { var x = 0\ndiscard comptime { x = 1 } }",
        "fn bad[T](x Int) Int { comptime { discard \"T\"\nx } }\nfn main() {}",
        "fn bad[T](x Int) Int { comptime { let y Int = comptime if T == Int { 1 } else { 2 }\nx + y } }\nfn main() {}",
        "fn flag[T](x Int) Bool { true }\nfn bad[T](x Int) Int { comptime if flag[T](x) { 1 } else { 0 } }\nfn main() {}",
        "fn bad[T](T Int) Int { comptime if T == Int { 1 } else { 0 } }\nfn main() {}",
        "fn main() { discard comptime { return } }",
        "fn impossible() Int requires false ensures result == 42 { 0 }\nfn main() { discard comptime { impossible() } }",
        "fn wrong() Int ensures result == 1 { 0 }\nfn main() { discard comptime { wrong() } }",
        "fn dependent[T]() Int ensures result == 0 { comptime if T == Int { 1 } else { 0 } }\nfn main() { discard dependent[Int]() }",
        "fn bad[T](x T) Int { let y Int = comptime if T == Int { x } else { 0 }\nx + y }\nfn main() { discard bad(1) }",
        "fn bad[T](x T) Int { comptime if T == Int { x } else { missing() } }\nfn main() { discard bad(true) }",
        "fn spin() Int { while true {} 0 }\nfn main() { discard comptime { spin() } }",
        "fn recurse() Int { recurse() }\nfn main() { discard comptime { recurse() } }",
        "fn cycle() Int { comptime { cycle() } }\nfn main() { discard cycle() }",
    ] {
        fs::write(source.path().join("main.loom"), text).unwrap();
        let output = loom(&["check", source.path().to_str().unwrap()]);
        assert_eq!(output.status.code(), Some(1), "accepted {text}: {output:?}");
        assert!(!output.stderr.is_empty());
    }
}

#[test]
fn compile_time_cannot_hide_io_in_a_dead_runtime_branch() {
    let source = tempfile::tempdir().unwrap();
    let marker = source.path().join("must-not-exist");
    fs::write(source.path().join("main.loom"), format!(
        "import std.file.write_text\nfn effect(flag Bool) Int {{ if flag {{ 1 }} else {{ discard write_text(\"{}\", \"bad\")\n2 }} }}\nfn main() {{ discard comptime {{ effect(true) }} }}", marker.display()
    )).unwrap();
    let output = loom(&["check", source.path().to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("compile-time"));
    assert!(!marker.exists());
}
