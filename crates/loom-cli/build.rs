use std::path::{Path, PathBuf};

#[path = "../../build-support/fingerprint.rs"]
mod fingerprint;

use fingerprint::{BuildFingerprint, assert_no_local_feature_table, emit_rerun_inputs};

const FRONTEND_CRATES: &[&str] = &[
    "loom-core",
    "loom-syntax",
    "loom-hir",
    "loom-sema",
    "loom-mir",
    "loom-lowering",
    "loom-driver",
];

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let workspace = manifest
        .parent()
        .and_then(Path::parent)
        .expect("loom-cli is nested below the workspace root");
    let mut inputs = vec![
        workspace.join("Cargo.toml"),
        workspace.join("Cargo.lock"),
        manifest.join("build.rs"),
        workspace.join("build-support/fingerprint.rs"),
        workspace.join("library/std"),
    ];
    for name in FRONTEND_CRATES {
        let root = workspace.join("crates").join(name);
        let crate_manifest = root.join("Cargo.toml");
        assert_no_local_feature_table(&crate_manifest).unwrap_or_else(|error| {
            panic!(
                "frontend build identity rejected {}: {error}",
                crate_manifest.display()
            )
        });
        assert!(
            !root.join("build.rs").exists(),
            "frontend crate {name} needs a crate-owned build identity before adding build.rs"
        );
        inputs.push(crate_manifest);
        inputs.push(root.join("src"));
    }
    inputs.sort();

    let mut identity = BuildFingerprint::new("loom-frontend-build-v1");
    for input in &inputs {
        emit_rerun_inputs(input)
            .unwrap_or_else(|error| panic!("watch build input {}: {error}", input.display()));
        identity
            .workspace_input(workspace, input)
            .unwrap_or_else(|error| panic!("fingerprint build input {}: {error}", input.display()));
    }
    identity
        .build_environment()
        .expect("fingerprint frontend Rust build environment");
    for name in ["HOST", "TARGET", "RUSTC"] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    println!(
        "cargo:rustc-env=LOOM_FRONTEND_BUILD_FINGERPRINT={}",
        identity.finish()
    );
}
