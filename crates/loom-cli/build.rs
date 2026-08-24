use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let workspace = manifest
        .parent()
        .and_then(Path::parent)
        .expect("loom-cli is nested below the workspace root");
    let mut inputs = Vec::new();
    println!(
        "cargo:rerun-if-changed={}",
        workspace.join("crates").display()
    );
    collect_files(&workspace.join("crates"), &mut inputs);
    for name in ["Cargo.toml", "Cargo.lock"] {
        inputs.push(workspace.join(name));
    }
    inputs.sort();

    let mut identity = Sha256::new();
    add_field(&mut identity, "domain", b"loom-compiler-source-v1");
    for path in inputs {
        println!("cargo:rerun-if-changed={}", path.display());
        let relative = path
            .strip_prefix(workspace)
            .expect("compiler input is below workspace")
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        add_field(&mut identity, "path", relative.as_bytes());
        let bytes = fs::read(&path)
            .unwrap_or_else(|error| panic!("could not fingerprint {}: {error}", path.display()));
        add_field(&mut identity, "bytes", &bytes);
    }
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let version = Command::new(rustc)
        .arg("--version")
        .output()
        .expect("rustc must report its version");
    assert!(version.status.success(), "rustc --version failed");
    add_field(&mut identity, "rustc", &version.stdout);
    println!(
        "cargo:rustc-env=LOOM_COMPILER_SOURCE_FINGERPRINT={:x}",
        identity.finalize()
    );
}

fn collect_files(directory: &Path, files: &mut Vec<PathBuf>) {
    let mut entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", directory.display()))
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|error| panic!("could not enumerate {}: {error}", directory.display()));
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .unwrap_or_else(|error| panic!("could not inspect {}: {error}", path.display()));
        if file_type.is_dir() {
            if matches!(
                entry.file_name().to_str(),
                Some("tests" | "benches" | "examples")
            ) {
                continue;
            }
            collect_files(&path, files);
        } else if file_type.is_file() && path.extension().is_some_and(|extension| extension == "rs")
        {
            files.push(path);
        }
    }
}

fn add_field(identity: &mut Sha256, label: &str, value: &[u8]) {
    identity.update(
        u64::try_from(label.len())
            .expect("label length fits u64")
            .to_be_bytes(),
    );
    identity.update(label.as_bytes());
    identity.update(
        u64::try_from(value.len())
            .expect("value length fits u64")
            .to_be_bytes(),
    );
    identity.update(value);
}
