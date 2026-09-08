use std::{
    fs,
    path::Path,
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};
mod common;
use common::success;

fn run(executable: &Path, inputs: &[&Path]) -> Output {
    let mut child = Command::new(executable)
        .args(inputs)
        .env("LOOM_GC_STRESS", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let limit = Instant::now() + Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= limit {
            child.kill().unwrap();
            panic!(
                "file completion failed to drain: {:?}",
                child.wait_with_output()
            );
        }
        thread::sleep(Duration::from_millis(5));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn source_file_tasks_roundtrip_binary_and_utf8_with_real_completion() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("输入-🙂.bin");
    let destination = directory.path().join("输出-é.bin");
    let payload: Vec<u8> = (0..70001).map(|index| (index % 256) as u8).collect();
    fs::write(&source, &payload).unwrap();
    let executable = common::executable(directory.path(), "async-files");
    let ir = directory.path().join("async-files.ll");
    for level in ["0", "2"] {
        success(
            &common::command(&[
                "build",
                "compiler/examples/async_files",
                "--output",
                executable.to_str().unwrap(),
                "--emit-ir",
                ir.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", level)
            .output()
            .unwrap(),
        );
        let output = run(&executable, &[&source, &destination]);
        success(&output);
        assert_eq!(output.stdout, b"70001 bytes copied\n");
        assert!(output.stderr.is_empty());
        assert_eq!(fs::read(&destination).unwrap(), payload);
        let ir = fs::read_to_string(&ir).unwrap();
        assert!(
            ir.contains("loom_rt_task_wait_file_read")
                && ir.contains("loom_rt_task_wait_file_write_bytes")
        );
    }

    let package = directory.path().join("package");
    fs::create_dir(&package).unwrap();
    fs::write(
        package.join("main.loom"),
        r#"
import std.file.tasks.read_text
import std.file.tasks.write_text
import std.file.FileError
import std.result.Result
import std.process.arguments
import std.text.concat
import std.time.sleep_ms

async fn check(path Text) {
    let payload = concat("line one\r\n", "界🙂\0end")
    assert match write_text(path, payload).await { Result.Ok(_) => true, Result.Err(_) => false }
    let first = read_text(path)
    let second = read_text(path)
    sleep_ms(1).await
    assert match first.await { Result.Ok(text) => text == payload, Result.Err(_) => false }
    assert match second.await { Result.Ok(text) => text == payload, Result.Err(_) => false }
    assert match read_text(concat(path, ".missing")).await {
        Result.Err(error) => error == FileError.Open
        Result.Ok(_) => false
    }
}
async fn main() {
    check(arguments()[1]).await
    assert match read_text(arguments()[2]).await {
        Result.Err(error) => error == FileError.Utf8
        Result.Ok(_) => false
    }
    discard write_text(arguments()[1], "").await
    assert match read_text(arguments()[1]).await { Result.Ok(text) => text == "", Result.Err(_) => false }
}
"#,
    )
    .unwrap();
    success(&common::loom(&["check", package.to_str().unwrap()]));
    success(&common::loom(&["test", "compiler/std/file/tasks"]));
    success(&common::loom(&[
        "build",
        package.to_str().unwrap(),
        "--output",
        executable.to_str().unwrap(),
    ]));
    success(&run(&executable, &[&destination, &source]));
}
