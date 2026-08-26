//! Target-derived native artifact naming.
//!
//! Artifact names follow the selected LLVM target, not the compiler host. This
//! matters both for native Windows builds and for an explicit Windows target
//! linked through a matching runtime bundle.

use std::path::{Path, PathBuf};

/// One filesystem artifact produced or consumed by the native pipeline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeArtifactKind {
    Executable,
    Object,
    StaticLibrary,
    DebugDatabase,
}

/// Returns whether the selected target uses the Windows executable format.
#[must_use]
pub fn target_uses_windows_artifacts(target_triple: Option<&str>) -> bool {
    target_triple.map_or(cfg!(windows), |triple| {
        triple
            .split('-')
            .any(|component| component.eq_ignore_ascii_case("windows"))
    })
}

/// Returns whether the selected target uses the MSVC object/link conventions.
#[must_use]
pub fn target_uses_msvc_artifacts(target_triple: Option<&str>) -> bool {
    target_triple.map_or(cfg!(target_env = "msvc"), |triple| {
        triple
            .split('-')
            .any(|component| component.eq_ignore_ascii_case("msvc"))
    })
}

/// Returns the conventional suffix for a selected target artifact.
#[must_use]
pub fn native_artifact_extension(
    target_triple: Option<&str>,
    kind: NativeArtifactKind,
) -> Option<&'static str> {
    match kind {
        NativeArtifactKind::Executable if target_uses_windows_artifacts(target_triple) => {
            Some("exe")
        }
        NativeArtifactKind::Object if target_uses_msvc_artifacts(target_triple) => Some("obj"),
        NativeArtifactKind::Object => Some("o"),
        NativeArtifactKind::StaticLibrary if target_uses_msvc_artifacts(target_triple) => {
            Some("lib")
        }
        NativeArtifactKind::StaticLibrary => Some("a"),
        NativeArtifactKind::DebugDatabase if target_uses_msvc_artifacts(target_triple) => {
            Some("pdb")
        }
        NativeArtifactKind::Executable | NativeArtifactKind::DebugDatabase => None,
    }
}

/// Applies the selected target's conventional suffix to one artifact path.
///
/// Explicit non-debug suffixes are preserved. Extensionless default and
/// temporary paths receive the target convention. Debug database paths replace
/// only an ASCII-case-insensitive `.exe` suffix; all other paths append `.pdb`
/// so an arbitrary requested output cannot collide with its debug companion.
#[must_use]
pub fn native_artifact_path(
    path: impl AsRef<Path>,
    target_triple: Option<&str>,
    kind: NativeArtifactKind,
) -> PathBuf {
    let path = path.as_ref();
    let Some(extension) = native_artifact_extension(target_triple, kind) else {
        return path.to_path_buf();
    };
    if kind == NativeArtifactKind::DebugDatabase {
        if path
            .extension()
            .and_then(std::ffi::OsStr::to_str)
            .is_some_and(|value| value.eq_ignore_ascii_case("exe"))
        {
            return path.with_extension(extension);
        }
        let mut appended = path.as_os_str().to_owned();
        appended.push(".");
        appended.push(extension);
        return PathBuf::from(appended);
    }
    if path.extension().is_some() {
        path.to_path_buf()
    } else {
        path.with_extension(extension)
    }
}

/// Returns the canonical runtime-bundle archive file name for a target.
#[must_use]
pub fn native_runtime_archive_name(target_triple: Option<&str>) -> &'static str {
    if target_uses_msvc_artifacts(target_triple) {
        "loom_runtime.lib"
    } else {
        "libloom_runtime.a"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_msvc_artifacts_use_pe_coff_conventions() {
        let target = Some("x86_64-pc-windows-msvc");
        assert_eq!(
            native_artifact_path(
                "target/loom/program",
                target,
                NativeArtifactKind::Executable
            ),
            Path::new("target/loom/program.exe")
        );
        assert_eq!(
            native_artifact_path("program", target, NativeArtifactKind::Object),
            Path::new("program.obj")
        );
        assert_eq!(native_runtime_archive_name(target), "loom_runtime.lib");
        assert_eq!(
            native_artifact_path("program.exe", target, NativeArtifactKind::DebugDatabase),
            Path::new("program.pdb")
        );
        assert_eq!(
            native_artifact_path("program.EXE", target, NativeArtifactKind::DebugDatabase),
            Path::new("program.pdb")
        );
        assert_eq!(
            native_artifact_path("program.bin", target, NativeArtifactKind::DebugDatabase),
            Path::new("program.bin.pdb")
        );
        assert_eq!(
            native_artifact_path("program.pdb", target, NativeArtifactKind::DebugDatabase),
            Path::new("program.pdb.pdb")
        );
        assert_eq!(
            native_artifact_path("program.PDB", target, NativeArtifactKind::DebugDatabase),
            Path::new("program.PDB.pdb")
        );
    }

    #[test]
    fn windows_gnu_keeps_gnu_object_and_archive_conventions() {
        let target = Some("x86_64-pc-windows-gnu");
        assert_eq!(
            native_artifact_path("program", target, NativeArtifactKind::Executable),
            Path::new("program.exe")
        );
        assert_eq!(
            native_artifact_path("program", target, NativeArtifactKind::Object),
            Path::new("program.o")
        );
        assert_eq!(native_runtime_archive_name(target), "libloom_runtime.a");
        assert_eq!(
            native_artifact_extension(target, NativeArtifactKind::DebugDatabase),
            None
        );
    }

    #[test]
    fn unix_artifacts_remain_extensionless_executables_with_o_and_a_inputs() {
        let target = Some("aarch64-apple-darwin");
        assert_eq!(
            native_artifact_path("program", target, NativeArtifactKind::Executable),
            Path::new("program")
        );
        assert_eq!(
            native_artifact_path("program", target, NativeArtifactKind::Object),
            Path::new("program.o")
        );
        assert_eq!(native_runtime_archive_name(target), "libloom_runtime.a");
    }

    #[test]
    fn explicit_nonstandard_suffixes_are_not_rewritten() {
        let target = Some("x86_64-pc-windows-msvc");
        assert_eq!(
            native_artifact_path("program.native", target, NativeArtifactKind::Executable),
            Path::new("program.native")
        );
        assert_eq!(
            native_artifact_path("program.o", target, NativeArtifactKind::Object),
            Path::new("program.o")
        );
    }
}
