use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

const GENERATED_FILE: &str = "stdlib_sources.rs";

fn main() -> io::Result<()> {
    let manifest = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo provides CARGO_MANIFEST_DIR"),
    );
    let workspace = manifest
        .parent()
        .and_then(Path::parent)
        .expect("loom-driver is nested below the workspace root");
    let std_root = workspace.join("library/std");
    println!("cargo:rerun-if-changed={}", std_root.display());

    let mut sources = Vec::new();
    collect_sources(&std_root, &std_root, &mut sources)?;
    sources.sort_by(|left, right| left.0.cmp(&right.0));

    let mut generated = String::from("const STD_SOURCES: &[StdSource] = &[\n");
    for (path, text) in sources {
        writeln!(
            &mut generated,
            "    StdSource {{ path: {path:?}, text: {text:?} }},"
        )
        .expect("writing generated Rust to a String cannot fail");
    }
    generated.push_str("];\n");

    let output = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo provides OUT_DIR"))
        .join(GENERATED_FILE);
    write_if_changed(&output, generated.as_bytes())
}

fn collect_sources(
    std_root: &Path,
    directory: &Path,
    sources: &mut Vec<(String, String)>,
) -> io::Result<()> {
    let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "compiler std source cannot be a symlink: {}",
                    path.display()
                ),
            ));
        }
        if metadata.is_dir() {
            collect_sources(std_root, &path, sources)?;
            continue;
        }
        if !metadata.is_file()
            || path.extension() != Some(OsStr::new("loom"))
            || path
                .file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| name.ends_with("_test.loom"))
        {
            continue;
        }

        let relative = path.strip_prefix(std_root).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "std source escaped {}: {}",
                    std_root.display(),
                    path.display()
                ),
            )
        })?;
        sources.push((portable_path(relative)?, fs::read_to_string(&path)?));
    }
    Ok(())
}

fn portable_path(path: &Path) -> io::Result<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        let Component::Normal(part) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("std source path is not relative: {}", path.display()),
            ));
        };
        parts.push(part.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("std source path is not UTF-8: {}", path.display()),
            )
        })?);
    }
    Ok(parts.join("/"))
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if fs::read(path).is_ok_and(|existing| existing == bytes) {
        return Ok(());
    }
    fs::write(path, bytes)
}
