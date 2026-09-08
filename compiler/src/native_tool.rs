//! Shared host linking and transactional output publication for native drivers.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn link(
    object: &Path,
    output: &Path,
    uses_runtime: bool,
    runtime: Option<&Path>,
    linker: Option<&OsStr>,
) -> Result<(), String> {
    let windows = cfg!(all(windows, target_env = "msvc"));
    let default_linker = std::env::var_os("LOOM_CC")
        .unwrap_or_else(|| if windows { "clang-cl" } else { "clang" }.into());
    let runtime = if uses_runtime {
        let runtime = runtime.map(Path::to_path_buf).unwrap_or_else(|| {
            std::env::var_os("LOOM_RUNTIME_LIBRARY")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    std::env::current_exe()
                        .unwrap_or_default()
                        .with_file_name(if windows {
                            "loom_runtime.lib"
                        } else {
                            "libloom_runtime.a"
                        })
                })
        });
        if !runtime.is_file() {
            return Err(format!(
                "missing native runtime {}: run cargo build --workspace or set LOOM_RUNTIME_LIBRARY",
                runtime.display()
            ));
        }
        Some(runtime)
    } else {
        None
    };
    let mut command = link_command(
        object,
        output,
        runtime.as_deref(),
        linker.unwrap_or(&default_linker),
        windows,
    );
    if uses_runtime && cfg!(target_os = "linux") {
        command.args(["-ldl", "-lpthread"]);
    }
    if cfg!(target_os = "linux") {
        // LLVM may lower scalar floating-point remainder to the host fmod.
        command.arg("-lm");
    }
    let result = command
        .output()
        .map_err(|error| format!("cannot start host linker: {error}"))?;
    if result.status.success() {
        Ok(())
    } else {
        Err(format!(
            "host linker failed: {}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        ))
    }
}

fn link_command(
    object: &Path,
    output: &Path,
    runtime: Option<&Path>,
    linker: &OsStr,
    windows: bool,
) -> Command {
    let mut command = Command::new(linker);
    if windows {
        command.arg("/nologo");
    }
    command.arg(object);
    if let Some(runtime) = runtime {
        command.arg(runtime);
    }
    if windows {
        let mut destination = OsString::from("/OUT:");
        destination.push(output);
        command.args([
            "/link",
            "/SUBSYSTEM:CONSOLE",
            "/INCREMENTAL:NO",
            "/OPT:REF",
            "/Brepro",
        ]);
        // Match the usual Unix main-stack capacity. Reserve virtual space;
        // leave the commit size unchanged so small programs pay only for use.
        command.arg("/STACK:8388608");
        command.arg(destination);
        // Native objects have no Clang-generated CRT .drectve section. Match
        // the runtime's static MSVC CRT, including scalar-only executables.
        command.args(["/DEFAULTLIB:libcmt", "/DEFAULTLIB:oldnames"]);
        if runtime.is_some() {
            command.args([
                "kernel32.lib",
                "ntdll.lib",
                "userenv.lib",
                "ws2_32.lib",
                "dbghelp.lib",
            ]);
        }
    } else {
        command.arg("-o").arg(output);
        // These are final executable links; library/object output bypasses this
        // path. Runtime static-library symbols are retained by native references.
        if cfg!(target_os = "macos") {
            command.arg("-Wl,-dead_strip");
        } else if cfg!(target_os = "linux") {
            command.arg("-Wl,--gc-sections");
        }
    }
    command
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
    if let Ok(existing) = path.canonicalize() {
        return Ok(existing);
    }
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
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            ["loom", "toml", "lock"]
                .iter()
                .any(|source| extension.eq_ignore_ascii_case(source))
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_extensions_are_protected_on_case_insensitive_filesystems() {
        for name in ["MAIN.LOOM", "loom.ToMl", "Cargo.LOCK"] {
            assert!(reject_source_output(Path::new(name), &[]).is_err());
        }
        assert!(reject_source_output(Path::new("program.exe"), &[]).is_ok());
    }

    #[test]
    fn windows_links_objects_with_explicit_crt_and_reproducible_output() {
        let object = Path::new("build files/program.obj");
        let output = Path::new("build files/program.exe");
        let runtime = Path::new("runtime files/loom_runtime.lib");
        let command = link_command(object, output, Some(runtime), OsStr::new("clang-cl"), true);
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args[1], object.as_os_str());
        assert_eq!(args[2], runtime.as_os_str());
        for argument in [
            "/OUT:build files/program.exe",
            "/DEFAULTLIB:libcmt",
            "/STACK:8388608",
            "/OPT:REF",
            "/Brepro",
            "dbghelp.lib",
        ] {
            assert!(args.contains(&OsStr::new(argument)));
        }
        assert!(!args.contains(&OsStr::new("-Wl,-dead_strip")));
        assert!(!args.contains(&OsStr::new("-Wl,--gc-sections")));
        let scalar = link_command(object, output, None, OsStr::new("clang-cl"), true);
        let args: Vec<_> = scalar.get_args().collect();
        assert!(args.contains(&OsStr::new("/DEFAULTLIB:libcmt")));
        assert!(args.contains(&OsStr::new("/OPT:REF")));
        assert!(!args.contains(&runtime.as_os_str()));
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn unix_executable_links_discard_unreferenced_sections() {
        let object = Path::new("build files/program.o");
        let output = Path::new("build files/program");
        let runtime = Path::new("runtime files/libloom_runtime.a");
        let flag = if cfg!(target_os = "macos") {
            "-Wl,-dead_strip"
        } else {
            "-Wl,--gc-sections"
        };
        for runtime in [None, Some(runtime)] {
            let command = link_command(object, output, runtime, OsStr::new("clang"), false);
            let args: Vec<_> = command.get_args().collect();
            assert_eq!(args[0], object.as_os_str());
            assert!(args.contains(&output.as_os_str()));
            assert!(args.contains(&OsStr::new(flag)));
            assert!(!args.contains(&OsStr::new("/OPT:REF")));
            if let Some(runtime) = runtime {
                assert_eq!(args[1], runtime.as_os_str());
            }
        }
    }
}
