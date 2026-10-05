use std::{fs, process::Command};
mod common;
use common::{loom, success};

#[test]
fn source_datagrams_use_real_readiness_packets_cancellation_and_moving_gc() {
    let example = "compiler/examples/udp_echo";
    for command in ["check", "test", "run"] {
        success(&loom(&[command, example]));
    }
    let directory = tempfile::tempdir().unwrap();
    let executable = common::executable(directory.path(), "udp-echo");
    let tests = common::executable(directory.path(), "udp-tests");
    let ir = directory.path().join("udp-echo.ll");
    for level in ["0", "2"] {
        success(
            &common::command(&[
                "build",
                example,
                "--output",
                executable.to_str().unwrap(),
                "--emit-ir",
                ir.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        success(&common::run_tasks(&executable));
        let text = fs::read_to_string(&ir).unwrap();
        assert!(text.contains("loom_rt_socket_receive_from"));
        assert!(text.contains("loom_rt_socket_send_to"));
        assert!(text.contains("loom_rt_task_wait_socket"));
        assert!(text.contains("loom_rt_socket_multicast_v4"));
        assert!(text.contains("loom_rt_socket_membership_v4"));
        assert!(text.contains("loom_rt_task_wait_resolve"));
        success(
            &common::command(&[
                "test",
                "compiler/std/net/udp",
                "--no-run",
                "--output",
                tests.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        success(&common::run_task_command(
            Command::new(&tests).env("LOOM_GC_STRESS", "1"),
        ));
    }
    let numeric = directory.path().join("numeric");
    fs::create_dir(&numeric).unwrap();
    fs::write(
        numeric.join("main.loom"),
        r#"
import std.net.udp.connect
import std.net.udp.close
import std.result.Result

async fn main() {
    match connect("127.0.0.1:9").await {
        Result.Ok(connection) => {
            assert match close(connection) {
                Result.Ok(_) => true
                Result.Err(_) => false
            }
        }
        Result.Err(_) => {
            assert false
        }
    }
}
"#,
    )
    .unwrap();
    success(
        &common::command(&[
            "build",
            numeric.to_str().unwrap(),
            "--output",
            executable.to_str().unwrap(),
            "--emit-ir",
            ir.to_str().unwrap(),
        ])
        .output()
        .unwrap(),
    );
    success(&common::run_tasks(&executable));
    assert!(
        !fs::read_to_string(ir)
            .unwrap()
            .contains("loom_rt_task_wait_resolve")
    );
}
