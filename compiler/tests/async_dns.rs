use std::fs;
mod common;
use common::success;

#[test]
fn source_dns_and_hostname_connect_use_completion_under_moving_gc() {
    let directory = tempfile::tempdir().unwrap();
    let executable = common::executable(directory.path(), "hostname-connect");
    let ir = directory.path().join("hostname-connect.ll");
    let package = "compiler/examples/hostname_connect";
    success(&common::loom(&["check", package]));
    for level in ["0", "2"] {
        for tests in ["compiler/std/net/dns", package] {
            eprintln!("O{level}: {tests}");
            success(
                &common::command(&["test", tests, "--no-run"])
                    .env("LOOM_OPT_LEVEL", level)
                    .output()
                    .unwrap(),
            );
            success(&common::run_tasks(&common::executable(
                &common::root().join(tests).join("target"),
                "tests",
            )));
        }
        success(
            &common::command(&[
                "build",
                package,
                "--output",
                executable.to_str().unwrap(),
                "--emit-ir",
                ir.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        let output = common::run_tasks(&executable);
        success(&output);
        assert_eq!(output.stdout, b"localhost connected\n");
        assert!(output.stderr.is_empty());
        let llvm = fs::read_to_string(&ir).unwrap();
        assert!(llvm.contains("loom_rt_task_wait_resolve"));
        assert!(llvm.contains("loom_rt_task_bytes_result"));
    }
    success(&common::run_task_command(&mut common::command(&[
        "run", package,
    ])));
}
