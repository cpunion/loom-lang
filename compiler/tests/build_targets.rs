use std::{fs, process::Command};

use loom_native::codegen::{Backend, EmitOptions, Llvm, Optimization};

mod common;
use common::{loom, success};

#[test]
fn observed_target_inputs_follow_native_emission_and_cache_revalidation() {
    let directory = tempfile::tempdir().unwrap();
    let executable = common::executable(directory.path(), "targeted");
    let cache = directory.path().join("cache");
    let example = "compiler/examples/build_target";
    let metadata = Command::new(env!("CARGO_BIN_EXE_loom-native"))
        .arg("--target-info")
        .output()
        .unwrap();
    success(&metadata);
    let metadata = String::from_utf8(metadata.stdout).unwrap();
    assert!(metadata.starts_with("loom-target 1\n"));
    assert!(metadata.contains(&format!("\nos {}\n", std::env::consts::OS)));
    assert!(metadata.contains(&format!("\narch {}\n", std::env::consts::ARCH)));
    assert!(metadata.contains("\npointer_width 64\nendian little\n"));
    success(&loom(&["check", example]));
    success(&loom(&["test", example]));
    let expected = format!(
        "{} / {}",
        match std::env::consts::OS {
            "macos" => "macOS",
            "windows" => "Windows",
            _ => "Linux",
        },
        std::env::consts::ARCH
    );
    let run = loom(&["run", example]);
    success(&run);
    assert_eq!(run.stdout, expected.as_bytes());
    for (level, cpu, hit) in [("0", "generic", false), ("2", "native", true)] {
        let output = common::command(&["build", example])
            .arg("--output")
            .arg(&executable)
            .arg("--frontend-cache")
            .arg(&cache)
            .env("LOOM_NATIVE_TIMINGS", "1")
            .env("LOOM_OPT_LEVEL", level)
            .env("LOOM_TARGET_CPU", cpu)
            .output()
            .unwrap();
        success(&output);
        assert!(String::from_utf8_lossy(&output.stderr).contains(if hit {
            "frontend hit"
        } else {
            "frontend miss"
        }));
        let run = Command::new(&executable).output().unwrap();
        success(&run);
        assert_eq!(run.stdout, expected.as_bytes());
    }
    // A target query cannot silently reuse an old frontend result when the
    // selected backend no longer supplies its metadata.
    let missing = common::command(&["build", example])
        .arg("--output")
        .arg(&executable)
        .arg("--frontend-cache")
        .arg(&cache)
        .arg("--native-tool")
        .arg(directory.path().join("missing-backend"))
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("cannot read target properties"));

    let checked = loom(&["emit-checked", example]);
    success(&checked);
    let mut program =
        loom_native::native_input::decode(std::str::from_utf8(&checked.stdout).unwrap()).unwrap();
    assert!(
        program
            .target_inputs
            .contains(&("os".into(), std::env::consts::OS.into()))
    );
    program.target_inputs = vec![("os".into(), "not-the-emission-target".into())];
    let object = directory.path().join("mismatched.o");
    let error = Llvm
        .emit(
            &program,
            EmitOptions {
                object: &object,
                ir: None,
                test_mode: false,
                optimization: Optimization::O0,
            },
        )
        .err()
        .unwrap();
    assert!(error.contains("does not match the emission target"));
    assert!(!object.exists());

    // Unselected queries and the imported declaration do not start a backend.
    fs::write(
        directory.path().join("main.loom"),
        r#"import std.build.target

fn main() {
    comptime if false {
        discard target("os")
    }
}
"#,
    )
    .unwrap();
    success(
        &common::command(&["check"])
            .arg(directory.path())
            .arg("--native-tool")
            .arg(directory.path().join("missing-backend"))
            .output()
            .unwrap(),
    );
}
