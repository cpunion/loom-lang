//! Shared host linking and transactional output publication for native drivers.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn link(
    object: &Path,
    output: &Path,
    uses_runtime: bool,
    runtime: Option<&Path>,
    linker: Option<&OsStr>,
) -> Result<(), String> {
    let default_linker = std::env::var_os("LOOM_CC").unwrap_or_else(|| "clang".into());
    let mut command = Command::new(linker.unwrap_or(&default_linker));
    command.arg(object);
    if uses_runtime {
        let runtime = runtime.map(Path::to_path_buf).unwrap_or_else(|| {
            std::env::var_os("LOOM_RUNTIME_LIBRARY")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    std::env::current_exe()
                        .unwrap_or_default()
                        .with_file_name("libloom_runtime.a")
                })
        });
        if !runtime.is_file() {
            return Err(format!(
                "missing native runtime {}: run cargo build --workspace or set LOOM_RUNTIME_LIBRARY",
                runtime.display()
            ));
        }
        command.arg(runtime);
        if cfg!(target_os = "linux") {
            command.args(["-ldl", "-lpthread", "-lm"]);
        }
    }
    let result = command
        .arg("-o")
        .arg(output)
        .output()
        .map_err(|error| format!("cannot start host linker: {error}"))?;
    if result.status.success() {
        Ok(())
    } else {
        Err(format!(
            "host linker failed: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        ))
    }
}

pub fn prepare_parent(path: &Path) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", path.display()))?;
    }
    Ok(())
}

pub fn output_identity(path: &Path) -> Result<PathBuf, String> {
    let absolute = std::path::absolute(path).map_err(|error| error.to_string())?;
    let parent = absolute.parent().ok_or("output needs a parent directory")?;
    Ok(parent
        .canonicalize()
        .map_err(|error| error.to_string())?
        .join(absolute.file_name().ok_or("output needs a file name")?))
}

pub fn publish(from: &Path, to: &Path) -> Result<(), String> {
    // Atomic directory-entry replacement also avoids modifying hard-linked inputs.
    let destination = output_identity(to)?;
    let staging = tempfile::NamedTempFile::new_in(destination.parent().unwrap())
        .map_err(|error| error.to_string())?;
    std::fs::copy(from, staging.path()).map_err(|error| error.to_string())?;
    staging
        .persist(destination)
        .map_err(|error| error.to_string())?;
    Ok(())
}

pub fn reject_source_output(path: &Path, sources: &[PathBuf]) -> Result<(), String> {
    if path.is_symlink() {
        return Err("native output paths cannot be symbolic links".into());
    }
    if path
        .extension()
        .is_some_and(|extension| extension == "loom" || extension == "toml" || extension == "lock")
    {
        return Err("native outputs cannot overwrite source, manifests, or lockfiles".into());
    }
    if let Ok(canonical) = path.canonicalize() {
        if sources
            .iter()
            .any(|source| source.canonicalize().ok().as_ref() == Some(&canonical))
        {
            return Err("native output aliases a source file".into());
        }
    }
    Ok(())
}
