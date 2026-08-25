use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=../loom-runtime-abi/Cargo.toml");
    println!("cargo:rerun-if-changed=../loom-runtime-abi/src/lib.rs");
    println!("cargo:rerun-if-changed=../loom-runtime/Cargo.toml");
    println!("cargo:rerun-if-changed=../loom-runtime/src/lib.rs");
    println!("cargo:rerun-if-changed=../loom-runtime/src/float.rs");
    println!("cargo:rerun-if-changed=../loom-runtime/src/gc.rs");
    println!("cargo:rerun-if-changed=../loom-runtime/src/int.rs");
    println!("cargo:rerun-if-changed=../loom-runtime/src/process.rs");
    println!("cargo:rerun-if-changed=../loom-runtime/src/reactor.rs");
    println!("cargo:rerun-if-changed=../loom-runtime/src/scheduler.rs");
    println!("cargo:rerun-if-changed=../loom-runtime/src/standard.rs");
    println!("cargo:rerun-if-changed=../loom-runtime/src/value.rs");

    let manifest = Path::new(&env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"))
        .join("../loom-runtime/Cargo.toml");
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("build output directory"));
    let target_dir = output.join("runtime-target");
    let target = env::var("TARGET").expect("target triple");
    let profile = env::var("PROFILE").expect("cargo profile");
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command
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
