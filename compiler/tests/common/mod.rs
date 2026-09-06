#![allow(dead_code)] // Integration test crates use different parts of this helper.

use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

pub fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap()
}

pub fn compiler() -> PathBuf {
    root().join("target/loom")
}

pub fn command(args: &[&str]) -> Command {
    let mut command = Command::new(compiler());
    command.args(args).current_dir(root());
    if args
        .first()
        .is_some_and(|mode| matches!(*mode, "check" | "build" | "test" | "run"))
    {
        command
            .arg("--std")
            .arg(root().join("compiler/std"))
            .args(["--native-tool", env!("CARGO_BIN_EXE_loom-native")]);
    }
    command
}

pub fn loom(args: &[&str]) -> Output {
    command(args)
        .output()
        .expect("build the Loom compiler with bash scripts/bootstrap.sh first")
}

pub fn success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

// Stress only the emitted program, not every allocation in the compiler itself.
pub fn managed(args: &[&str], directory: &Path) -> Output {
    let temporary = tempfile::tempdir().unwrap();
    let artifact = if args[0] == "test" {
        success(&loom(args));
        Path::new(args[1]).join("target/tests")
    } else {
        assert_eq!(args[0], "run");
        let artifact = temporary.path().join("app");
        success(&loom(&[
            "build",
            args[1],
            "--output",
            artifact.to_str().unwrap(),
        ]));
        artifact
    };
    Command::new(artifact)
        .env("LOOM_GC_STRESS", "1")
        .current_dir(directory)
        .output()
        .unwrap()
}
