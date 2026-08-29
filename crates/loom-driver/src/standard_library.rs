//! Compiler-owned Loom source library embedded into every project graph.

use std::path::{Path, PathBuf};

use loom_core::PackageId;
use sha2::{Digest, Sha256};

use crate::project::ProjectSource;

pub(crate) use loom_core::STD_PACKAGE_NAME;
const STANDARD_LIBRARY_IDENTITY_DOMAIN: &str = "loom-source-stdlib-v1";

struct StandardSource {
    path: &'static str,
    module: &'static str,
    text: &'static str,
}

const STANDARD_SOURCES: &[StandardSource] = &[
    StandardSource {
        path: "src/int.loom",
        module: "std.int",
        text: include_str!("../../../library/std/src/int.loom"),
    },
    StandardSource {
        path: "src/log.loom",
        module: "std.log",
        text: include_str!("../../../library/std/src/log.loom"),
    },
    StandardSource {
        path: "src/resource.loom",
        module: "std.resource",
        text: include_str!("../../../library/std/src/resource.loom"),
    },
];

#[must_use]
pub(crate) fn package_id(language_version: &str) -> PackageId {
    PackageId::compiler_std(language_version)
}

#[must_use]
pub(crate) fn project_sources(root: &Path, language_version: &str) -> Vec<ProjectSource> {
    let package = package_id(language_version);
    let synthetic_root = synthetic_root(root, language_version);
    STANDARD_SOURCES
        .iter()
        .map(|source| ProjectSource {
            absolute: synthetic_root.join(source.path),
            stable_path: format!("deps/{package}/{}", source.path),
            package: Some(package.clone()),
            is_root_package: false,
            embedded_text: Some(source.text.to_owned()),
            origin: crate::SourceOrigin::CompilerOwnedStandardLibrary,
        })
        .collect()
}

/// Whether a module belongs to the compiler-reserved standard namespace.
///
/// Reserving the complete namespace lets future source modules be added without
/// colliding with a user module that happened to claim the same path first.
#[must_use]
pub(crate) fn owns_module(module: &str) -> bool {
    module == STD_PACKAGE_NAME
        || module
            .strip_prefix(STD_PACKAGE_NAME)
            .is_some_and(|suffix| suffix.starts_with('.'))
}

#[must_use]
pub fn identity(language_version: &str) -> String {
    identity_for_sources(language_version, STANDARD_SOURCES)
}

fn synthetic_root(root: &Path, language_version: &str) -> PathBuf {
    root.join("target")
        .join("loom")
        .join("compiler-owned")
        .join(STD_PACKAGE_NAME)
        .join(language_version)
}

fn identity_for_sources(language_version: &str, sources: &[StandardSource]) -> String {
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, STANDARD_LIBRARY_IDENTITY_DOMAIN.as_bytes());
    hash_field(&mut hasher, language_version.as_bytes());
    for source in sources {
        hash_field(&mut hasher, source.path.as_bytes());
        hash_field(&mut hasher, source.module.as_bytes());
        hash_field(&mut hasher, source.text.as_bytes());
    }
    format!("{STANDARD_LIBRARY_IDENTITY_DOMAIN}/{:x}", hasher.finalize())
}

fn hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(value);
}

#[cfg(test)]
mod tests {
    use super::{
        STANDARD_LIBRARY_IDENTITY_DOMAIN, StandardSource, identity_for_sources, owns_module,
    };

    fn source(path: &'static str, module: &'static str, text: &'static str) -> StandardSource {
        StandardSource { path, module, text }
    }

    #[test]
    fn identity_tracks_language_paths_and_contents() {
        let base = identity_for_sources("0.3", &[source("src/a.loom", "std.a", "module std.a\n")]);
        assert!(base.starts_with(&format!("{STANDARD_LIBRARY_IDENTITY_DOMAIN}/")));
        assert_eq!(base.len(), STANDARD_LIBRARY_IDENTITY_DOMAIN.len() + 1 + 64);
        assert_ne!(
            base,
            identity_for_sources("0.4", &[source("src/a.loom", "std.a", "module std.a\n")],)
        );
        assert_ne!(
            base,
            identity_for_sources("0.3", &[source("src/b.loom", "std.a", "module std.a\n")],)
        );
        assert_ne!(
            base,
            identity_for_sources(
                "0.3",
                &[source("src/a.loom", "std.changed", "module std.changed\n",)],
            )
        );
    }

    #[test]
    fn identity_tracks_exact_source_bytes_at_the_same_path_and_module() {
        let base = identity_for_sources(
            "0.3",
            &[source(
                "src/int.loom",
                "std.int",
                "module std.int\n\npub fn parse_int(text Text) Int { 1 }\n",
            )],
        );
        let changed_body = identity_for_sources(
            "0.3",
            &[source(
                "src/int.loom",
                "std.int",
                "module std.int\n\npub fn parse_int(text Text) Int { 2 }\n",
            )],
        );

        assert_ne!(base, changed_body);
    }

    #[test]
    fn complete_std_namespace_is_reserved_without_matching_prefixes() {
        assert!(owns_module("std"));
        assert!(owns_module("std.resource"));
        assert!(owns_module("std.future.nested"));
        assert!(!owns_module("stdish.resource"));
        assert!(!owns_module("application.std"));
    }
}
