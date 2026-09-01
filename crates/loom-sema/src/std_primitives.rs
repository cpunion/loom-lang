//! Compiler-private primitives available only to their owning `std` wrappers.
//!
//! Public standard-library declarations resolve through ordinary source
//! definitions. These identities authorize only the irreducible calls inside
//! the exact compiler-owned wrapper module; they are not public import aliases.

use loom_hir::{ModuleId, Path, Program};

const PROCESS_MODULE: &str = "std.process";
const IO_MODULE: &str = "std.io";
const FLOAT_MODULE: &str = "std.float";
const FILE_MODULE: &str = "std.file";
const LOG_MODULE: &str = "std.log";
const NET_MODULE: &str = "std.net";
const TASK_MODULE: &str = "std.task";

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum CompilerStdPrimitive {
    FloatFromInt,
    FloatFormat,
    FloatParseStatus,
    FloatToInt,
    FileOpenRead,
    FileCreate,
    FileTryOpenRead,
    FileTryCreate,
    FileReadText,
    FileWriteText,
    FileTryReadText,
    FileTryWriteText,
    FileClose,
    IoWriteStdout,
    LogWrite,
    SocketConnect,
    SocketTryConnect,
    SocketReadText,
    SocketWriteText,
    SocketTryReadText,
    SocketTryWriteText,
    SocketClose,
    ProcessArgumentCount,
    ProcessArgumentAt,
    ProcessEnvironment,
    TaskSleep,
}

impl CompilerStdPrimitive {
    #[must_use]
    pub(crate) const fn local_name(self) -> &'static str {
        match self {
            Self::FloatFromInt => "__from_int",
            Self::FloatFormat => "__format",
            Self::FloatParseStatus => "__parse",
            Self::FloatToInt => "__to_int",
            Self::FileOpenRead => "__open_read",
            Self::FileCreate => "__create",
            Self::FileTryOpenRead => "__try_open_read",
            Self::FileTryCreate => "__try_create",
            Self::FileReadText | Self::SocketReadText => "__read_text",
            Self::FileWriteText | Self::SocketWriteText => "__write_text",
            Self::FileTryReadText | Self::SocketTryReadText => "__try_read_text",
            Self::FileTryWriteText | Self::SocketTryWriteText => "__try_write_text",
            Self::FileClose | Self::SocketClose => "__close",
            Self::IoWriteStdout => "__write_stdout",
            Self::LogWrite => "__write",
            Self::SocketConnect => "__connect",
            Self::SocketTryConnect => "__try_connect",
            Self::ProcessArgumentCount => "__argument_count",
            Self::ProcessArgumentAt => "__argument_at",
            Self::ProcessEnvironment => "__environment",
            Self::TaskSleep => "__sleep",
        }
    }
}

/// Resolves one exact primitive import after authenticating its owner.
///
/// The structural segment match deliberately avoids treating a hostile HIR
/// segment containing dots as equivalent to an explicit three-segment import.
#[must_use]
pub(crate) fn resolve_import(
    program: &Program,
    owner: ModuleId,
    path: &Path,
) -> Option<CompilerStdPrimitive> {
    let module = &program.modules[owner];
    if !module.package.is_compiler_std() {
        return None;
    }

    let [package, namespace, item] = path.segments.as_slice() else {
        return None;
    };
    if package.name.as_str() != "std" {
        return None;
    }
    match (
        module.name.as_str(),
        namespace.name.as_str(),
        item.name.as_str(),
    ) {
        (FLOAT_MODULE, "float", "__from_int") => Some(CompilerStdPrimitive::FloatFromInt),
        (FLOAT_MODULE, "float", "__format") => Some(CompilerStdPrimitive::FloatFormat),
        (FLOAT_MODULE, "float", "__parse") => Some(CompilerStdPrimitive::FloatParseStatus),
        (FLOAT_MODULE, "float", "__to_int") => Some(CompilerStdPrimitive::FloatToInt),
        (FILE_MODULE, "file", "__open_read") => Some(CompilerStdPrimitive::FileOpenRead),
        (FILE_MODULE, "file", "__create") => Some(CompilerStdPrimitive::FileCreate),
        (FILE_MODULE, "file", "__try_open_read") => Some(CompilerStdPrimitive::FileTryOpenRead),
        (FILE_MODULE, "file", "__try_create") => Some(CompilerStdPrimitive::FileTryCreate),
        (FILE_MODULE, "file", "__read_text") => Some(CompilerStdPrimitive::FileReadText),
        (FILE_MODULE, "file", "__write_text") => Some(CompilerStdPrimitive::FileWriteText),
        (FILE_MODULE, "file", "__try_read_text") => Some(CompilerStdPrimitive::FileTryReadText),
        (FILE_MODULE, "file", "__try_write_text") => Some(CompilerStdPrimitive::FileTryWriteText),
        (FILE_MODULE, "file", "__close") => Some(CompilerStdPrimitive::FileClose),
        (IO_MODULE, "io", "__write_stdout") => Some(CompilerStdPrimitive::IoWriteStdout),
        (LOG_MODULE, "log", "__write") => Some(CompilerStdPrimitive::LogWrite),
        (NET_MODULE, "net", "__connect") => Some(CompilerStdPrimitive::SocketConnect),
        (NET_MODULE, "net", "__try_connect") => Some(CompilerStdPrimitive::SocketTryConnect),
        (NET_MODULE, "net", "__read_text") => Some(CompilerStdPrimitive::SocketReadText),
        (NET_MODULE, "net", "__write_text") => Some(CompilerStdPrimitive::SocketWriteText),
        (NET_MODULE, "net", "__try_read_text") => Some(CompilerStdPrimitive::SocketTryReadText),
        (NET_MODULE, "net", "__try_write_text") => Some(CompilerStdPrimitive::SocketTryWriteText),
        (NET_MODULE, "net", "__close") => Some(CompilerStdPrimitive::SocketClose),
        (PROCESS_MODULE, "process", "__argument_count") => {
            Some(CompilerStdPrimitive::ProcessArgumentCount)
        }
        (PROCESS_MODULE, "process", "__argument_at") => {
            Some(CompilerStdPrimitive::ProcessArgumentAt)
        }
        (PROCESS_MODULE, "process", "__environment") => {
            Some(CompilerStdPrimitive::ProcessEnvironment)
        }
        (TASK_MODULE, "task", "__sleep") => Some(CompilerStdPrimitive::TaskSleep),
        _ => None,
    }
}

/// Finds the primitive explicitly imported for one unqualified wrapper call.
#[must_use]
pub(crate) fn resolve_local_call(
    program: &Program,
    owner: ModuleId,
    path: &Path,
) -> Option<CompilerStdPrimitive> {
    let [name] = path.segments.as_slice() else {
        return None;
    };
    program.modules[owner].imports.iter().find_map(|import| {
        if import.file != name.span.file {
            return None;
        }
        let primitive = resolve_import(program, owner, &import.path)?;
        (name.name.as_str() == primitive.local_name()).then_some(primitive)
    })
}

#[cfg(test)]
mod tests {
    use loom_core::{FileId, LOOM_LANGUAGE_VERSION, ModuleName, Name, PackageId, Span};
    use loom_hir::{Path, PathSegment, Program};

    use super::{CompilerStdPrimitive, resolve_import};

    fn path(segments: &[&str]) -> Path {
        let span = Span::new(FileId(0), 0, 1);
        Path {
            segments: segments
                .iter()
                .map(|segment| PathSegment {
                    name: Name::new(*segment),
                    span,
                })
                .collect(),
        }
    }

    fn module(program: &mut Program, package: PackageId, name: &str) -> loom_hir::ModuleId {
        program.intern_package_module(
            package,
            ModuleName::new(name),
            FileId(0),
            Span::new(FileId(0), 0, 1),
        )
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one authority matrix covers every compiler-private std primitive owner"
    )]
    fn primitives_require_exact_package_owner_and_segments() {
        let mut program = Program::default();
        let std_package = PackageId::compiler_std(LOOM_LANGUAGE_VERSION);
        let owner = module(&mut program, std_package.clone(), "std.process");
        let io_owner = module(&mut program, std_package.clone(), "std.io");
        let float_owner = module(&mut program, std_package.clone(), "std.float");
        let file_owner = module(&mut program, std_package.clone(), "std.file");
        let log_owner = module(&mut program, std_package.clone(), "std.log");
        let net_owner = module(&mut program, std_package.clone(), "std.net");
        let task_owner = module(&mut program, std_package.clone(), "std.task");
        let wrong_owner = module(&mut program, std_package.clone(), "std.other");
        let wrong_package = module(&mut program, PackageId::standalone(), "std.process");
        let wrong_file_package = module(&mut program, PackageId::standalone(), "std.file");
        let wrong_net_package = module(&mut program, PackageId::standalone(), "std.net");
        let wrong_task_package = module(&mut program, PackageId::standalone(), "std.task");

        assert_eq!(
            resolve_import(
                &program,
                owner,
                &path(&["std", "process", "__argument_count"]),
            ),
            Some(CompilerStdPrimitive::ProcessArgumentCount)
        );
        assert_eq!(
            resolve_import(&program, owner, &path(&["std", "process", "__argument_at"]),),
            Some(CompilerStdPrimitive::ProcessArgumentAt)
        );
        assert_eq!(
            resolve_import(
                &program,
                float_owner,
                &path(&["std", "float", "__from_int"]),
            ),
            Some(CompilerStdPrimitive::FloatFromInt)
        );
        assert_eq!(
            resolve_import(&program, float_owner, &path(&["std", "float", "__parse"]),),
            Some(CompilerStdPrimitive::FloatParseStatus)
        );
        assert_eq!(
            resolve_import(&program, float_owner, &path(&["std", "float", "__format"]),),
            Some(CompilerStdPrimitive::FloatFormat)
        );
        assert_eq!(
            resolve_import(&program, float_owner, &path(&["std", "float", "__to_int"]),),
            Some(CompilerStdPrimitive::FloatToInt)
        );
        assert_eq!(
            resolve_import(&program, io_owner, &path(&["std", "io", "__write_stdout"]),),
            Some(CompilerStdPrimitive::IoWriteStdout)
        );
        assert_eq!(
            resolve_import(&program, file_owner, &path(&["std", "file", "__open_read"]),),
            Some(CompilerStdPrimitive::FileOpenRead)
        );
        assert_eq!(
            resolve_import(&program, file_owner, &path(&["std", "file", "__create"]),),
            Some(CompilerStdPrimitive::FileCreate)
        );
        assert_eq!(
            resolve_import(
                &program,
                file_owner,
                &path(&["std", "file", "__try_open_read"]),
            ),
            Some(CompilerStdPrimitive::FileTryOpenRead)
        );
        assert_eq!(
            resolve_import(
                &program,
                file_owner,
                &path(&["std", "file", "__try_create"]),
            ),
            Some(CompilerStdPrimitive::FileTryCreate)
        );
        for (name, primitive) in [
            ("__read_text", CompilerStdPrimitive::FileReadText),
            ("__write_text", CompilerStdPrimitive::FileWriteText),
            ("__try_read_text", CompilerStdPrimitive::FileTryReadText),
            ("__try_write_text", CompilerStdPrimitive::FileTryWriteText),
            ("__close", CompilerStdPrimitive::FileClose),
        ] {
            assert_eq!(
                resolve_import(&program, file_owner, &path(&["std", "file", name])),
                Some(primitive)
            );
        }
        assert_eq!(
            resolve_import(&program, log_owner, &path(&["std", "log", "__write"]),),
            Some(CompilerStdPrimitive::LogWrite)
        );
        assert_eq!(
            resolve_import(&program, net_owner, &path(&["std", "net", "__connect"]),),
            Some(CompilerStdPrimitive::SocketConnect)
        );
        assert_eq!(
            resolve_import(&program, net_owner, &path(&["std", "net", "__try_connect"]),),
            Some(CompilerStdPrimitive::SocketTryConnect)
        );
        for (name, primitive) in [
            ("__read_text", CompilerStdPrimitive::SocketReadText),
            ("__write_text", CompilerStdPrimitive::SocketWriteText),
            ("__try_read_text", CompilerStdPrimitive::SocketTryReadText),
            ("__try_write_text", CompilerStdPrimitive::SocketTryWriteText),
            ("__close", CompilerStdPrimitive::SocketClose),
        ] {
            assert_eq!(
                resolve_import(&program, net_owner, &path(&["std", "net", name])),
                Some(primitive)
            );
        }
        assert_eq!(
            resolve_import(&program, owner, &path(&["std", "process", "__environment"]),),
            Some(CompilerStdPrimitive::ProcessEnvironment)
        );
        assert_eq!(
            resolve_import(&program, task_owner, &path(&["std", "task", "__sleep"])),
            Some(CompilerStdPrimitive::TaskSleep)
        );

        for (candidate_owner, candidate_path) in [
            (wrong_owner, path(&["std", "process", "__argument_count"])),
            (wrong_package, path(&["std", "process", "__argument_count"])),
            (wrong_file_package, path(&["std", "file", "__open_read"])),
            (wrong_net_package, path(&["std", "net", "__connect"])),
            (wrong_task_package, path(&["std", "task", "__sleep"])),
            (owner, path(&["std.process", "__argument_count"])),
            (owner, path(&["std", "process", "arguments"])),
            (
                owner,
                path(&["std", "process", "__argument_count", "extra"]),
            ),
            (owner, path(&["std", "process", "__arguments"])),
            (owner, path(&["std", "io", "__write_stdout"])),
            (io_owner, path(&["std", "process", "__argument_count"])),
            (io_owner, path(&["std.io", "__write_stdout"])),
            (io_owner, path(&["std", "io", "write"])),
            (owner, path(&["std", "io", "__error_kind"])),
            (io_owner, path(&["std", "io", "__error_kind"])),
            (io_owner, path(&["std.io", "__error_kind"])),
            (io_owner, path(&["std", "io", "error_kind"])),
            (io_owner, path(&["std", "io", "__error_kind", "extra"])),
            (owner, path(&["std", "io", "__error_message"])),
            (io_owner, path(&["std", "io", "__error_message"])),
            (io_owner, path(&["std.io", "__error_message"])),
            (io_owner, path(&["std", "io", "error_message"])),
            (io_owner, path(&["std", "io", "__error_message", "extra"])),
            (owner, path(&["std", "log", "__write"])),
            (log_owner, path(&["std.log", "__write"])),
            (log_owner, path(&["std", "log", "write"])),
            (log_owner, path(&["std", "log", "__write", "extra"])),
            (owner, path(&["std", "float", "__from_int"])),
            (owner, path(&["std", "file", "__open_read"])),
            (file_owner, path(&["std.file", "__open_read"])),
            (file_owner, path(&["std", "file", "open_read"])),
            (file_owner, path(&["std", "file", "__open_read", "extra"])),
            (owner, path(&["std", "file", "__close"])),
            (file_owner, path(&["std", "net", "__close"])),
            (file_owner, path(&["std", "net", "__connect"])),
            (net_owner, path(&["std.net", "__connect"])),
            (net_owner, path(&["std", "net", "connect"])),
            (net_owner, path(&["std", "net", "__connect", "extra"])),
            (owner, path(&["std", "net", "__read_text"])),
            (net_owner, path(&["std", "file", "__read_text"])),
            (net_owner, path(&["std", "file", "__open_read"])),
            (float_owner, path(&["std", "float", "from_int"])),
            (float_owner, path(&["std", "float", "parse_float"])),
            (float_owner, path(&["std", "float", "format_float"])),
            (float_owner, path(&["std", "float", "is_finite"])),
            (float_owner, path(&["std", "float", "__is_finite"])),
            (float_owner, path(&["std.float", "__from_int"])),
            (float_owner, path(&["std", "float", "__to_int", "extra"])),
            (owner, path(&["std", "task", "__sleep"])),
            (task_owner, path(&["std.task", "__sleep"])),
            (task_owner, path(&["std", "task", "sleep"])),
            (task_owner, path(&["std", "task", "__sleep", "extra"])),
        ] {
            assert_eq!(
                resolve_import(&program, candidate_owner, &candidate_path),
                None
            );
        }
    }
}
