//! Platform linker command construction.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use crate::native_artifact::{
    NativeArtifactKind, native_artifact_path, target_uses_msvc_artifacts,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LinkerFlavor {
    GnuDriver,
    ClangCl,
    MsvcLinker,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NativeLinkCommand {
    pub(crate) arguments: Vec<OsString>,
    pub(crate) pdb: Option<PathBuf>,
}

pub(crate) fn linker_flavor(program: &Path) -> LinkerFlavor {
    let stem = program
        .file_stem()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    match stem.as_str() {
        "link" | "lld-link" => LinkerFlavor::MsvcLinker,
        "cl" | "clang-cl" => LinkerFlavor::ClangCl,
        _ => LinkerFlavor::GnuDriver,
    }
}

pub(crate) fn linker_version_arguments(program: &Path) -> &'static [&'static str] {
    match linker_flavor(program) {
        LinkerFlavor::MsvcLinker => &["/HELP"],
        LinkerFlavor::GnuDriver | LinkerFlavor::ClangCl => &["--version"],
    }
}

pub(crate) fn native_runtime_link_args(target_triple: &str) -> Vec<String> {
    if triple_has_component(target_triple, "linux") {
        ["-ldl", "-lpthread", "-lm", "-lrt", "-lutil"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    } else if triple_has_component(target_triple, "windows") {
        [
            "-lkernel32",
            "-lntdll",
            "-luserenv",
            "-lws2_32",
            "-ldbghelp",
            "-lmsvcrt",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    } else {
        Vec::new()
    }
}

pub(crate) fn native_link_command(
    program: &Path,
    target_triple: &str,
    object: &Path,
    runtimes: &[&Path],
    link_args: &[String],
    output: &Path,
) -> NativeLinkCommand {
    let flavor = linker_flavor(program);
    let msvc_target = target_uses_msvc_artifacts(Some(target_triple));
    let pdb = msvc_target.then(|| {
        native_artifact_path(
            output,
            Some(target_triple),
            NativeArtifactKind::DebugDatabase,
        )
    });
    let mut arguments = Vec::with_capacity(link_args.len() + runtimes.len() + 8);
    arguments.push(object.as_os_str().to_owned());
    arguments.extend(
        runtimes
            .iter()
            .map(|runtime| runtime.as_os_str().to_owned()),
    );
    arguments.extend(link_args.iter().map(|argument| {
        if matches!(flavor, LinkerFlavor::MsvcLinker | LinkerFlavor::ClangCl) {
            msvc_link_argument(argument)
        } else {
            OsString::from(argument)
        }
    }));

    match flavor {
        LinkerFlavor::MsvcLinker => {
            arguments.push(prefixed_path("/OUT:", output));
            if let Some(pdb) = &pdb {
                arguments.push(OsString::from("/DEBUG"));
                arguments.push(prefixed_path("/PDB:", pdb));
            }
        }
        LinkerFlavor::ClangCl => {
            arguments.push(prefixed_path("/Fe", output));
            if let Some(pdb) = &pdb {
                arguments.push(OsString::from("/link"));
                arguments.push(OsString::from("/DEBUG"));
                arguments.push(prefixed_path("/PDB:", pdb));
            }
        }
        LinkerFlavor::GnuDriver => {
            if let Some(pdb) = &pdb {
                arguments.push(OsString::from("-Wl,/DEBUG"));
                let mut argument = OsString::from("-Wl,/PDB:");
                argument.push(pdb);
                arguments.push(argument);
            }
            arguments.push(OsString::from("-o"));
            arguments.push(output.as_os_str().to_owned());
        }
    }

    NativeLinkCommand { arguments, pdb }
}

fn triple_has_component(target_triple: &str, expected: &str) -> bool {
    target_triple
        .split('-')
        .any(|component| component.eq_ignore_ascii_case(expected))
}

fn prefixed_path(prefix: &str, path: &Path) -> OsString {
    let mut value = OsString::from(prefix);
    value.push(path);
    value
}

fn msvc_link_argument(argument: &str) -> OsString {
    argument
        .strip_prefix("-l")
        .filter(|library| !library.is_empty())
        .map_or_else(
            || OsString::from(argument),
            |library| OsString::from(format!("{library}.lib")),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(arguments: &[OsString]) -> Vec<String> {
        arguments
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn msvc_linker_receives_obj_lib_out_and_pdb_arguments() {
        let link_args = native_runtime_link_args("x86_64-pc-windows-msvc");
        let command = native_link_command(
            Path::new("lld-link.exe"),
            "x86_64-pc-windows-msvc",
            Path::new("program.obj"),
            &[Path::new("loom_runtime.lib")],
            &link_args,
            Path::new("program.exe"),
        );
        assert_eq!(
            text(&command.arguments),
            [
                "program.obj",
                "loom_runtime.lib",
                "kernel32.lib",
                "ntdll.lib",
                "userenv.lib",
                "ws2_32.lib",
                "dbghelp.lib",
                "msvcrt.lib",
                "/OUT:program.exe",
                "/DEBUG",
                "/PDB:program.pdb",
            ]
        );
        assert_eq!(command.pdb.as_deref(), Some(Path::new("program.pdb")));
    }

    #[test]
    fn clang_driver_receives_windows_pdb_without_changing_driver_output_syntax() {
        let link_args = native_runtime_link_args("x86_64-pc-windows-msvc");
        let command = native_link_command(
            Path::new("clang.exe"),
            "x86_64-pc-windows-msvc",
            Path::new("program.obj"),
            &[Path::new("loom_runtime.lib")],
            &link_args,
            Path::new("program.exe"),
        );
        assert_eq!(
            text(&command.arguments),
            [
                "program.obj",
                "loom_runtime.lib",
                "-lkernel32",
                "-lntdll",
                "-luserenv",
                "-lws2_32",
                "-ldbghelp",
                "-lmsvcrt",
                "-Wl,/DEBUG",
                "-Wl,/PDB:program.pdb",
                "-o",
                "program.exe",
            ]
        );
    }

    #[test]
    fn clang_cl_uses_driver_output_then_linker_pdb_arguments() {
        let command = native_link_command(
            Path::new("clang-cl.exe"),
            "x86_64-pc-windows-msvc",
            Path::new("program.obj"),
            &[Path::new("loom_runtime.lib")],
            &["-luserenv".to_owned()],
            Path::new("program.exe"),
        );
        assert_eq!(
            text(&command.arguments),
            [
                "program.obj",
                "loom_runtime.lib",
                "userenv.lib",
                "/Feprogram.exe",
                "/link",
                "/DEBUG",
                "/PDB:program.pdb",
            ]
        );
    }

    #[test]
    fn unix_link_command_keeps_existing_driver_order() {
        let command = native_link_command(
            Path::new("clang"),
            "aarch64-apple-darwin",
            Path::new("program.o"),
            &[Path::new("libloom_runtime.a")],
            &[],
            Path::new("program"),
        );
        assert_eq!(
            text(&command.arguments),
            ["program.o", "libloom_runtime.a", "-o", "program"]
        );
        assert_eq!(command.pdb, None);
    }

    #[test]
    fn msvc_linkers_use_their_help_probe() {
        assert_eq!(linker_version_arguments(Path::new("link.exe")), ["/HELP"]);
        assert_eq!(
            linker_version_arguments(Path::new("lld-link.exe")),
            ["/HELP"]
        );
        assert_eq!(
            linker_version_arguments(Path::new("clang-cl.exe")),
            ["--version"]
        );
    }

    #[test]
    fn windows_runtime_libraries_match_rustc_native_static_lib_order() {
        assert_eq!(
            native_runtime_link_args("x86_64-pc-windows-msvc"),
            [
                "-lkernel32",
                "-lntdll",
                "-luserenv",
                "-lws2_32",
                "-ldbghelp",
                "-lmsvcrt",
            ]
        );
    }
}
