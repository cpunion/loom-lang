use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    for input in [
        "../../Cargo.toml",
        "../../Cargo.lock",
        "../loom-runtime-abi/Cargo.toml",
        "../loom-runtime-abi/src",
        "../loom-runtime/Cargo.toml",
        "../loom-runtime/src",
    ] {
        emit_rerun_inputs(Path::new(input));
    }

    let manifest = Path::new(&env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"))
        .join("../loom-runtime/Cargo.toml");
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("build output directory"));
    let target_dir = output.join("runtime-target");
    let target = env::var("TARGET").expect("target triple");
    let profile = env::var("PROFILE").expect("cargo profile");
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command
        // The embedded runtime is exported and reused across machines which share a target
        // triple. Do not let the compiler build's local tuning leak into that portable archive.
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env("CARGO_ENCODED_RUSTFLAGS", "-Ctarget-cpu=generic")
        .arg("build")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--package")
        .arg("loom-runtime")
        .arg("--target-dir")
        .arg(&target_dir)
        .arg("--target")
        .arg(&target);
    if profile == "release" {
        command.arg("--release");
    }
    let status = command.status().expect("run Cargo for Loom native runtime");
    assert!(status.success(), "failed to build Loom native runtime");

    let candidates = target_dir.join(&target).join(&profile).join("deps");
    let archive = fs::read_dir(&candidates)
        .expect("read Loom runtime artifacts")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension() == Some(OsStr::new("a"))
                && path
                    .file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| name.starts_with("libloom_runtime-"))
        })
        .max_by_key(|path| {
            fs::metadata(path)
                .and_then(|metadata| metadata.modified())
                .ok()
        })
        .expect("find Loom runtime static library");
    fs::copy(archive, output.join("libloom_runtime.a")).expect("copy Loom runtime static library");
}

fn emit_rerun_inputs(path: &Path) {
    println!("cargo:rerun-if-changed={}", path.display());
    if !path.is_dir() {
        return;
    }

    let mut entries = fs::read_dir(path)
        .unwrap_or_else(|error| panic!("read runtime input directory {}: {error}", path.display()))
        .map(|entry| {
            entry
                .unwrap_or_else(|error| {
                    panic!("read runtime input below {}: {error}", path.display())
                })
                .path()
        })
        .collect::<Vec<_>>();
    entries.sort();
    for entry in entries {
        emit_rerun_inputs(&entry);
    }
}
