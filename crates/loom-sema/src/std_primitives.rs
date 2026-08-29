//! Compiler-private primitives available only to their owning `std` wrappers.
//!
//! Public standard-library declarations resolve through ordinary source
//! definitions. These identities authorize only the irreducible calls inside
//! the exact compiler-owned wrapper module; they are not public import aliases.

use loom_hir::{ModuleId, Path, Program};

const PROCESS_MODULE: &str = "std.process";

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum CompilerStdPrimitive {
    ProcessArguments,
    ProcessEnvironment,
}

impl CompilerStdPrimitive {
    #[must_use]
    pub(crate) const fn local_name(self) -> &'static str {
        match self {
            Self::ProcessArguments => "__arguments",
            Self::ProcessEnvironment => "__environment",
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
    if !module.package.is_compiler_std() || module.name.as_str() != PROCESS_MODULE {
        return None;
    }

    let [package, process, item] = path.segments.as_slice() else {
        return None;
    };
    if package.name.as_str() != "std" || process.name.as_str() != "process" {
        return None;
    }
    match item.name.as_str() {
        "__arguments" => Some(CompilerStdPrimitive::ProcessArguments),
        "__environment" => Some(CompilerStdPrimitive::ProcessEnvironment),
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
    fn process_primitives_require_exact_package_owner_and_segments() {
        let mut program = Program::default();
        let std_package = PackageId::compiler_std(LOOM_LANGUAGE_VERSION);
        let owner = module(&mut program, std_package.clone(), "std.process");
        let wrong_owner = module(&mut program, std_package, "std.other");
        let wrong_package = module(&mut program, PackageId::standalone(), "std.process");

        assert_eq!(
            resolve_import(&program, owner, &path(&["std", "process", "__arguments"]),),
            Some(CompilerStdPrimitive::ProcessArguments)
        );
        assert_eq!(
            resolve_import(&program, owner, &path(&["std", "process", "__environment"]),),
            Some(CompilerStdPrimitive::ProcessEnvironment)
        );

        for (candidate_owner, candidate_path) in [
            (wrong_owner, path(&["std", "process", "__arguments"])),
            (wrong_package, path(&["std", "process", "__arguments"])),
            (owner, path(&["std.process", "__arguments"])),
            (owner, path(&["std", "process", "arguments"])),
            (owner, path(&["std", "process", "__arguments", "extra"])),
        ] {
            assert_eq!(
                resolve_import(&program, candidate_owner, &candidate_path),
                None
            );
        }
    }
}
