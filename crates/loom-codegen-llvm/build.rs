use std::env;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

#[path = "../../build-support/fingerprint.rs"]
mod fingerprint;

use fingerprint::{BuildFingerprint, assert_no_local_feature_table, emit_rerun_inputs};

const OBJECT_CRATES: &[&str] = &[
    "loom-codegen-ir",
    "loom-codegen-llvm",
    "loom-core",
    "loom-mir",
    "loom-runtime-abi",
];

fn main() {
    emit_object_build_fingerprint();
    for input in [
        "../../Cargo.toml",
        "../../Cargo.lock",
        "../loom-runtime-abi/Cargo.toml",
        "../loom-runtime-abi/src",
        "../loom-runtime/Cargo.toml",
        "../loom-runtime/src",
    ] {
        emit_rerun_inputs(Path::new(input))
            .unwrap_or_else(|error| panic!("watch runtime input {input}: {error}"));
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

fn emit_object_build_fingerprint() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let workspace = manifest
        .parent()
        .and_then(Path::parent)
        .expect("loom-codegen-llvm is nested below the workspace root");
    let mut inputs = vec![
        workspace.join("Cargo.toml"),
        workspace.join("Cargo.lock"),
        manifest.join("build.rs"),
        workspace.join("build-support/fingerprint.rs"),
    ];
    for name in OBJECT_CRATES {
        let root = workspace.join("crates").join(name);
        let crate_manifest = root.join("Cargo.toml");
        if *name != "loom-codegen-llvm" {
            assert_no_local_feature_table(&crate_manifest).unwrap_or_else(|error| {
                panic!(
                    "object build identity rejected {}: {error}",
                    crate_manifest.display()
                )
            });
            assert!(
                !root.join("build.rs").exists(),
                "object dependency {name} needs a crate-owned build identity before adding build.rs"
            );
        }
        inputs.push(crate_manifest);
        inputs.push(root.join("src"));
    }
    inputs.sort();

    let mut identity = BuildFingerprint::new("loom-llvm-object-build-v1");
    for input in &inputs {
        emit_rerun_inputs(input)
            .unwrap_or_else(|error| panic!("watch build input {}: {error}", input.display()));
        identity
            .workspace_input(workspace, input)
            .unwrap_or_else(|error| panic!("fingerprint build input {}: {error}", input.display()));
    }
    identity
        .build_environment()
        .expect("fingerprint LLVM object Rust build environment");
    add_llvm_toolchain_identity(&mut identity);
    for name in ["HOST", "TARGET", "RUSTC"] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    println!(
        "cargo:rustc-env=LOOM_LLVM_OBJECT_BUILD_FINGERPRINT={}",
        identity.finish()
    );
}

fn add_llvm_toolchain_identity(identity: &mut BuildFingerprint) {
    let declared_libdir = PathBuf::from(
        env::var_os("DEP_LLVM_19_LIBDIR")
            .expect("direct llvm-sys dependency must provide DEP_LLVM_19_LIBDIR"),
    );
    let declared_config = PathBuf::from(
        env::var_os("DEP_LLVM_19_CONFIG_PATH")
            .expect("direct llvm-sys dependency must provide DEP_LLVM_19_CONFIG_PATH"),
    );
    let config = if declared_config.is_absolute() {
        declared_config
    } else if declared_config.components().count() == 1 {
        declared_libdir
            .parent()
            .expect("LLVM libdir must have an installation prefix")
            .join("bin")
            .join(declared_config)
    } else {
        panic!(
            "llvm-sys returned ambiguous relative llvm-config path {}",
            declared_config.display()
        )
    };
    let config = add_external_file(identity, "llvm-config", &config);

    for option in [
        "--version",
        "--host-target",
        "--build-mode",
        "--assertion-mode",
        "--targets-built",
        "--shared-mode",
    ] {
        let output = llvm_config(&config, option);
        identity.field("llvm-config-option", option.as_bytes());
        identity.field("llvm-config-output", &output);
    }

    let reported_libdir_output = String::from_utf8(llvm_config(&config, "--libdir"))
        .expect("llvm-config --libdir must be UTF-8");
    let reported_libdir_text = reported_libdir_output.trim();
    let reported_libdir = PathBuf::from(reported_libdir_text);
    let declared_libdir = fs::canonicalize(&declared_libdir).unwrap_or_else(|error| {
        panic!("resolve LLVM libdir {}: {error}", declared_libdir.display())
    });
    let reported_libdir = fs::canonicalize(&reported_libdir).unwrap_or_else(|error| {
        panic!("resolve LLVM libdir {}: {error}", reported_libdir.display())
    });
    assert_eq!(
        declared_libdir, reported_libdir,
        "llvm-sys metadata and its exact llvm-config disagree on libdir"
    );

    let (link_mode, names_output) = select_llvm_link_mode(&config);
    identity.field("llvm-link-mode", link_mode.as_bytes());
    identity.field("llvm-libnames", &names_output);
    let libfiles = String::from_utf8(llvm_config_args(&config, &["--libfiles", link_mode]))
        .expect("llvm-config --libfiles must be UTF-8");
    let normalized_libfiles = libfiles.replace(reported_libdir_text, "$LLVM_LIBDIR");
    assert_ne!(
        normalized_libfiles, libfiles,
        "llvm-config --libfiles did not use its reported libdir"
    );
    identity.field("llvm-libfiles", normalized_libfiles.as_bytes());

    let names = String::from_utf8(names_output).expect("llvm-config --libnames must be UTF-8");
    let mut names = names.split_ascii_whitespace().collect::<Vec<_>>();
    names.sort_unstable();
    names.dedup();
    assert!(
        !names.is_empty(),
        "llvm-config returned no linked LLVM libraries"
    );
    for name in names {
        assert_eq!(
            Path::new(name).file_name().and_then(OsStr::to_str),
            Some(name),
            "llvm-config returned a non-local library name"
        );
        add_external_file(identity, "libllvm", &declared_libdir.join(name));
    }
}

fn llvm_config(config: &Path, option: &str) -> Vec<u8> {
    llvm_config_args(config, &[option])
}

fn select_llvm_link_mode(config: &Path) -> (&'static str, Vec<u8>) {
    let modes: &[&str] = if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        &["--link-static"]
    } else {
        &["--link-shared", "--link-static"]
    };
    let mut failures = Vec::new();
    for &mode in modes {
        match try_llvm_config(config, &["--libnames", mode]) {
            Ok(output) => return (mode, output),
            Err(error) => failures.push(error),
        }
    }
    panic!(
        "{} could not select the llvm-sys prefer-dynamic link mode: {}",
        config.display(),
        failures.join("; ")
    );
}

fn llvm_config_args(config: &Path, arguments: &[&str]) -> Vec<u8> {
    try_llvm_config(config, arguments).unwrap_or_else(|error| panic!("{error}"))
}

fn try_llvm_config(config: &Path, arguments: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new(config)
        .args(arguments)
        .output()
        .map_err(|error| format!("run {} {}: {error}", config.display(), arguments.join(" ")))?;
    if output.stdout.len().saturating_add(output.stderr.len()) > 1024 * 1024 {
        return Err(format!(
            "{} {} returned oversized output",
            config.display(),
            arguments.join(" ")
        ));
    }
    if !output.status.success() {
        return Err(format!(
            "{} {} failed: {}",
            config.display(),
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    if !output.stdout.iter().any(|byte| !byte.is_ascii_whitespace()) {
        return Err(format!(
            "{} {} returned empty output",
            config.display(),
            arguments.join(" ")
        ));
    }
    Ok(output.stdout)
}

fn add_external_file(identity: &mut BuildFingerprint, role: &str, path: &Path) -> PathBuf {
    assert!(
        path.is_absolute(),
        "external build input {} must be absolute",
        path.display()
    );
    println!("cargo:rerun-if-changed={}", path.display());
    let resolved = fs::canonicalize(path)
        .unwrap_or_else(|error| panic!("resolve external build input {}: {error}", path.display()));
    if resolved != path {
        println!("cargo:rerun-if-changed={}", resolved.display());
    }
    let metadata = fs::symlink_metadata(&resolved).unwrap_or_else(|error| {
        panic!(
            "inspect external build input {}: {error}",
            resolved.display()
        )
    });
    assert!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "external build input {} is not a regular file",
        resolved.display()
    );
    let mut file = File::open(&resolved).unwrap_or_else(|error| {
        panic!("open external build input {}: {error}", resolved.display())
    });
    let mut digest = Sha256::new();
    let mut bytes = 0_u64;
    let mut chunk = vec![0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut chunk).unwrap_or_else(|error| {
            panic!("hash external build input {}: {error}", resolved.display())
        });
        if count == 0 {
            break;
        }
        bytes = bytes.saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
        digest.update(&chunk[..count]);
    }
    identity.field("external-role", role.as_bytes());
    identity.field(
        "external-name",
        resolved
            .file_name()
            .unwrap_or_else(|| OsStr::new(""))
            .as_encoded_bytes(),
    );
    identity.field("external-size", &bytes.to_be_bytes());
    identity.field("external-sha256", &digest.finalize());
    resolved
}
