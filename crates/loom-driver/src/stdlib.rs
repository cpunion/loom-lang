//! Compiler-owned Loom `std` sources embedded into every project graph.

use std::path::{Path, PathBuf};

use loom_core::PackageId;
use sha2::{Digest, Sha256};

use crate::project::ProjectSource;

pub(crate) use loom_core::STD_PACKAGE_NAME;
const STDLIB_IDENTITY_DOMAIN: &str = "loom-source-stdlib-v2";

struct StdSource {
    path: &'static str,
    text: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/stdlib_sources.rs"));

#[must_use]
pub(crate) fn package_id(language_version: &str) -> PackageId {
    PackageId::compiler_std(language_version)
}

#[must_use]
pub(crate) fn project_sources(root: &Path, language_version: &str) -> Vec<ProjectSource> {
    let package = package_id(language_version);
    let synthetic_root = synthetic_root(root, language_version);
    STD_SOURCES
        .iter()
        .map(|source| {
            let module =
                crate::project::package_module_name(&package, std::path::Path::new(source.path))
                    .expect("compiler std paths are valid package paths");
            ProjectSource {
                absolute: synthetic_root.join(source.path),
                stable_path: format!("deps/{package}/{}", source.path),
                package: Some(package.clone()),
                module,
                is_root_package: false,
                embedded_text: Some(source.text.to_owned()),
                origin: crate::SourceOrigin::CompilerStd,
                participation: crate::SourceParticipation::Production,
            }
        })
        .collect()
}

#[must_use]
pub fn identity(language_version: &str) -> String {
    identity_for_sources(language_version, STD_SOURCES)
}

fn synthetic_root(root: &Path, language_version: &str) -> PathBuf {
    root.join("target")
        .join("loom")
        .join("compiler-owned")
        .join(STD_PACKAGE_NAME)
        .join(language_version)
}

fn identity_for_sources(language_version: &str, sources: &[StdSource]) -> String {
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, STDLIB_IDENTITY_DOMAIN.as_bytes());
    hash_field(&mut hasher, language_version.as_bytes());
    for source in sources {
        hash_field(&mut hasher, source.path.as_bytes());
        hash_field(&mut hasher, source.text.as_bytes());
    }
    format!("{STDLIB_IDENTITY_DOMAIN}/{:x}", hasher.finalize())
}

fn hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(value);
}

#[cfg(test)]
mod tests {
    use super::{STD_SOURCES, STDLIB_IDENTITY_DOMAIN, StdSource, identity_for_sources};

    fn source(path: &'static str, text: &'static str) -> StdSource {
        StdSource { path, text }
    }

    #[test]
    fn identity_tracks_language_paths_and_contents() {
        let base = identity_for_sources("0.4", &[source("a/a.loom", "pub fn value() Int { 1 }\n")]);
        assert!(base.starts_with(&format!("{STDLIB_IDENTITY_DOMAIN}/")));
        assert_eq!(base.len(), STDLIB_IDENTITY_DOMAIN.len() + 1 + 64);
        assert_ne!(
            base,
            identity_for_sources("0.3", &[source("a/a.loom", "pub fn value() Int { 1 }\n")],)
        );
        assert_ne!(
            base,
            identity_for_sources("0.4", &[source("b/b.loom", "pub fn value() Int { 1 }\n")],)
        );
    }

    #[test]
    fn identity_tracks_exact_source_bytes_at_the_same_path_and_module() {
        let base = identity_for_sources(
            "0.4",
            &[source(
                "int/int.loom",
                "pub fn parse_int(text Text) Int { 1 }\n",
            )],
        );
        let changed_body = identity_for_sources(
            "0.4",
            &[source(
                "int/int.loom",
                "pub fn parse_int(text Text) Int { 2 }\n",
            )],
        );

        assert_ne!(base, changed_body);
    }

    #[test]
    fn embedded_sources_have_unique_sorted_portable_paths() {
        assert!(STD_SOURCES.iter().all(|source| {
            let path = std::path::Path::new(source.path);
            path.extension() == Some(std::ffi::OsStr::new("loom"))
                && !path
                    .file_stem()
                    .and_then(std::ffi::OsStr::to_str)
                    .is_some_and(|stem| stem.ends_with("_test"))
                && !source.path.contains('\\')
        }));
        assert!(
            STD_SOURCES
                .windows(2)
                .all(|sources| sources[0].path < sources[1].path)
        );
    }
}
