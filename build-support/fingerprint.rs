use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

pub struct BuildFingerprint {
    identity: Sha256,
}

impl BuildFingerprint {
    pub fn new(domain: &str) -> Self {
        let mut fingerprint = Self {
            identity: Sha256::new(),
        };
        fingerprint.field("domain", domain.as_bytes());
        fingerprint
    }

    pub fn field(&mut self, label: &str, value: &[u8]) {
        add_field(&mut self.identity, label, value);
    }

    pub fn workspace_input(&mut self, workspace: &Path, path: &Path) -> io::Result<()> {
        let mut files = Vec::new();
        collect_regular_files(path, &mut files)?;
        files.sort();
        for file in files {
            let relative = file.strip_prefix(workspace).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("build input {} is outside the workspace", file.display()),
                )
            })?;
            self.field("input-path", portable_path(relative)?.as_bytes());
            self.field("input-bytes", &fs::read(&file)?);
        }
        Ok(())
    }

    pub fn build_environment(&mut self) -> io::Result<()> {
        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let output = Command::new(rustc).arg("-vV").output()?;
        if !output.status.success() || output.stdout.is_empty() {
            return Err(io::Error::other(
                "rustc -vV did not return a toolchain identity",
            ));
        }
        self.field("rustc-vv-stdout", &output.stdout);
        self.field("rustc-vv-stderr", &output.stderr);
        for name in ["HOST", "TARGET"] {
            let value = std::env::var_os(name).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("Cargo did not provide {name}"),
                )
            })?;
            self.field("build-env-name", name.as_bytes());
            self.field("build-env-value", value.as_encoded_bytes());
        }
        let mut semantic = std::env::vars_os()
            .filter(|(name, _)| {
                let name = name.to_string_lossy();
                name.starts_with("CARGO_CFG_") || name.starts_with("CARGO_FEATURE_")
            })
            .collect::<Vec<_>>();
        semantic.sort_by(|left, right| left.0.cmp(&right.0));
        for (name, value) in semantic {
            self.field("build-env-name", name.as_encoded_bytes());
            self.field("build-env-value", value.as_encoded_bytes());
        }
        Ok(())
    }

    pub fn finish(self) -> String {
        format!("{:x}", self.identity.finalize())
    }
}

pub fn emit_rerun_inputs(path: &Path) -> io::Result<()> {
    println!("cargo:rerun-if-changed={}", path.display());
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "workspace build input {} cannot be a symlink",
                path.display()
            ),
        ));
    }
    if !metadata.is_dir() {
        return Ok(());
    }
    let mut entries = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        emit_rerun_inputs(&entry.path())?;
    }
    Ok(())
}

pub fn assert_no_local_feature_table(manifest: &Path) -> io::Result<()> {
    let source = fs::read_to_string(manifest)?;
    let document = toml::from_str::<toml::Table>(&source).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid Cargo manifest {}: {error}", manifest.display()),
        )
    })?;
    if document.contains_key("features") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} declares local features; add a crate-owned build identity before enabling them",
                manifest.display()
            ),
        ));
    }
    Ok(())
}

fn collect_regular_files(path: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "workspace build input {} cannot be a symlink",
                path.display()
            ),
        ));
    }
    if metadata.is_file() {
        files.push(path.to_path_buf());
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("workspace build input {} is not regular", path.display()),
        ));
    }
    let mut entries = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        collect_regular_files(&entry.path(), files)?;
    }
    Ok(())
}

fn portable_path(path: &Path) -> io::Result<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        let part = component.as_os_str().to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("build input path {} is not valid UTF-8", path.display()),
            )
        })?;
        parts.push(part);
    }
    Ok(parts.join("/"))
}

fn add_field(identity: &mut Sha256, label: &str, value: &[u8]) {
    identity.update(
        u64::try_from(label.len())
            .expect("fingerprint label length fits u64")
            .to_be_bytes(),
    );
    identity.update(label.as_bytes());
    identity.update(
        u64::try_from(value.len())
            .expect("fingerprint value length fits u64")
            .to_be_bytes(),
    );
    identity.update(value);
}
