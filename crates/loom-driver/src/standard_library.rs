//! Compiler-owned Loom source library embedded into every project graph.

use std::path::{Path, PathBuf};

use loom_core::PackageId;
use sha2::{Digest, Sha256};

use crate::project::ProjectSource;

pub(crate) const STANDARD_PACKAGE_NAME: &str = "standard";
const STANDARD_LIBRARY_IDENTITY_DOMAIN: &str = "loom-source-stdlib-v1";

struct StandardSource {
    path: &'static str,
    module: &'static str,
    text: &'static str,
}

const STANDARD_SOURCES: &[StandardSource] = &[StandardSource {
    path: "src/int.loom",
    module: "standard.int",
    text: include_str!("../../../library/standard/src/int.loom"),
}];

#[must_use]
pub(crate) fn package_id(language_version: &str) -> PackageId {
    PackageId::with_language(STANDARD_PACKAGE_NAME, language_version, language_version)
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

/// Whether a module is supplied by the current compiler-owned source library.
///
/// Legacy projects may still contain historical compiler-known namespaces
/// such as `standard.resource`; only an actually distributed source module is
/// reserved here, so the reservation grows atomically with the embedded source
/// set.
#[must_use]
pub(crate) fn owns_module(module: &str) -> bool {
    STANDARD_SOURCES
        .iter()
        .any(|source| source.module == module)
}

#[must_use]
pub fn identity(language_version: &str) -> String {
    identity_for_sources(language_version, STANDARD_SOURCES)
}

fn synthetic_root(root: &Path, language_version: &str) -> PathBuf {
    root.join("target")
        .join("loom")
        .join("compiler-owned")
        .join(STANDARD_PACKAGE_NAME)
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
    use super::{STANDARD_LIBRARY_IDENTITY_DOMAIN, StandardSource, identity_for_sources};

    fn source(path: &'static str, module: &'static str, text: &'static str) -> StandardSource {
        StandardSource { path, module, text }
    }

    #[test]
    fn identity_tracks_language_paths_and_contents() {
        let base = identity_for_sources(
            "0.3",
            &[source("src/a.loom", "standard.a", "module standard.a\n")],
        );
        assert!(base.starts_with(&format!("{STANDARD_LIBRARY_IDENTITY_DOMAIN}/")));
        assert_eq!(base.len(), STANDARD_LIBRARY_IDENTITY_DOMAIN.len() + 1 + 64);
        assert_ne!(
            base,
            identity_for_sources(
                "0.4",
                &[source("src/a.loom", "standard.a", "module standard.a\n")],
            )
        );
        assert_ne!(
            base,
            identity_for_sources(
                "0.3",
                &[source("src/b.loom", "standard.a", "module standard.a\n")],
            )
        );
        assert_ne!(
            base,
            identity_for_sources(
                "0.3",
                &[source(
                    "src/a.loom",
                    "standard.changed",
                    "module standard.changed\n",
                )],
            )
        );
    }
}
