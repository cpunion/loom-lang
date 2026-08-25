//! Portable, validated package artifacts consumed by manifest dependencies.
//!
//! A `.loomlib` is deliberately not a native ABI. It carries the resolved
//! package graph, canonical public-interface fingerprints, compiler-private
//! source payloads, and validated checked MIR. Source payloads let the current
//! compiler instantiate generics and re-run proofs without requiring the
//! producer checkout; only the public interface is part of the user-facing
//! dependency surface.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use loom_core::{LOOM_LANGUAGE_VERSION, PackageId};
use loom_mir::{CheckedProgram, Program, decode_interpreted_artifact, encode_interpreted_artifact};
use loom_syntax::parse_with_file;
use serde::{Deserialize, Serialize};

use crate::incremental::module_interfaces;
use crate::{ModuleInterface, ProjectGraph, SourceMap};

/// Wire format name for portable package artifacts.
pub const LIBRARY_ARTIFACT_FORMAT: &str = "loom-library";
/// Version of the package/interface envelope (independent of checked MIR).
pub const LIBRARY_ARTIFACT_VERSION: u32 = 1;

/// A decoded `.loomlib` which has crossed both package and MIR validation.
#[derive(Clone, Debug)]
pub struct LibraryArtifact {
    pub(crate) root_package: PackageId,
    pub(crate) packages: Vec<LibraryPackage>,
    pub(crate) sources: Vec<LibrarySource>,
    interfaces: Vec<ModuleInterface>,
    program: CheckedProgram,
}

impl LibraryArtifact {
    #[must_use]
    pub const fn root_package(&self) -> &PackageId {
        &self.root_package
    }

    #[must_use]
    pub fn interfaces(&self) -> &[ModuleInterface] {
        &self.interfaces
    }

    #[must_use]
    pub const fn program(&self) -> &CheckedProgram {
        &self.program
    }

    pub(crate) fn into_dependency_parts(
        self,
    ) -> (PackageId, Vec<LibraryPackage>, Vec<LibrarySource>) {
        (self.root_package, self.packages, self.sources)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct LibraryPackage {
    pub id: PackageId,
    pub dependencies: Vec<LibraryDependency>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct LibraryDependency {
    pub alias: String,
    pub requirement: Option<String>,
    pub package: PackageId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct LibrarySource {
    pub package: PackageId,
    pub path: String,
    pub text: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LibraryEnvelope {
    format: String,
    version: u32,
    language_version: String,
    root_package: PackageId,
    packages: Vec<LibraryPackage>,
    public_interfaces: Vec<ModuleInterface>,
    sources: Vec<LibrarySource>,
    checked_mir: String,
}

/// A fail-closed portable library codec error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LibraryArtifactError {
    InvalidJson(String),
    FormatMismatch { found: String },
    VersionMismatch { found: u32 },
    LanguageVersionMismatch { found: String },
    InvalidGraph(String),
    InvalidMir(String),
}

impl LibraryArtifactError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::LanguageVersionMismatch { .. } => "ArtifactLanguageVersionMismatch",
            Self::FormatMismatch { .. } | Self::VersionMismatch { .. } => {
                "LibraryArtifactVersionMismatch"
            }
            Self::InvalidJson(_) | Self::InvalidGraph(_) | Self::InvalidMir(_) => {
                "InvalidLibraryArtifact"
            }
        }
    }
}

impl fmt::Display for LibraryArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson(message) => write!(formatter, "invalid .loomlib JSON: {message}"),
            Self::FormatMismatch { found } => write!(
                formatter,
                "library artifact format `{found}` is incompatible with `{LIBRARY_ARTIFACT_FORMAT}`"
            ),
            Self::VersionMismatch { found } => write!(
                formatter,
                "library artifact version {found} is incompatible with {LIBRARY_ARTIFACT_VERSION}"
            ),
            Self::LanguageVersionMismatch { found } => write!(
                formatter,
                "library language version `{found}` is incompatible with `{LOOM_LANGUAGE_VERSION}`"
            ),
            Self::InvalidGraph(message) => {
                write!(formatter, "invalid library package graph: {message}")
            }
            Self::InvalidMir(message) => {
                write!(formatter, "invalid library checked MIR: {message}")
            }
        }
    }
}

impl std::error::Error for LibraryArtifactError {}

/// Encodes one resolved package graph as a deterministic portable library.
///
/// # Errors
///
/// Returns an error if the graph has no manifest root, source provenance is
/// incomplete, or checked MIR cannot cross its normal artifact boundary.
pub fn encode_library_artifact(
    project: &ProjectGraph,
    sources: &SourceMap,
    program: &Program,
) -> Result<Vec<u8>, LibraryArtifactError> {
    let root_package = project
        .root_package()
        .ok_or_else(|| {
            LibraryArtifactError::InvalidGraph(
                "portable libraries require a manifest package".to_owned(),
            )
        })?
        .id()
        .clone();
    let packages = project
        .packages()
        .map(|package| LibraryPackage {
            id: package.id().clone(),
            dependencies: package
                .dependencies()
                .iter()
                .map(|dependency| LibraryDependency {
                    alias: dependency.alias().to_owned(),
                    requirement: dependency.requirement().map(str::to_owned),
                    package: dependency.package().clone(),
                })
                .collect(),
        })
        .collect::<Vec<_>>();
    let mut parses = BTreeMap::new();
    let mut embedded_sources = Vec::new();
    for source in sources.documents() {
        let package = source.package().cloned().ok_or_else(|| {
            LibraryArtifactError::InvalidGraph(format!(
                "source `{}` has no package provenance",
                source.relative_path()
            ))
        })?;
        let text = source.text().ok_or_else(|| {
            LibraryArtifactError::InvalidGraph(format!(
                "source `{}` is not valid UTF-8",
                source.relative_path()
            ))
        })?;
        parses.insert(source.id(), parse_with_file(source.id(), text));
        embedded_sources.push(LibrarySource {
            path: package_relative_path(source.relative_path(), &package),
            package,
            text: text.to_owned(),
        });
    }
    embedded_sources.sort_by(|left, right| {
        left.package
            .cmp(&right.package)
            .then(left.path.cmp(&right.path))
    });
    let interfaces = module_interfaces(sources, &parses);
    let checked_mir = encode_interpreted_artifact(program)
        .map_err(|error| LibraryArtifactError::InvalidMir(error.to_string()))?;
    let checked_mir = String::from_utf8(checked_mir)
        .map_err(|error| LibraryArtifactError::InvalidMir(error.to_string()))?;
    serde_json::to_vec(&LibraryEnvelope {
        format: LIBRARY_ARTIFACT_FORMAT.to_owned(),
        version: LIBRARY_ARTIFACT_VERSION,
        language_version: project.language_version().to_owned(),
        root_package,
        packages,
        public_interfaces: interfaces,
        sources: embedded_sources,
        checked_mir,
    })
    .map_err(|error| LibraryArtifactError::InvalidJson(error.to_string()))
}

/// Decodes and validates a portable library and its nested checked MIR.
///
/// # Errors
///
/// Rejects incompatible formats/languages, malformed package graphs, private
/// path escapes, duplicate identities, and MIR failing ordinary validation.
pub fn decode_library_artifact(bytes: &[u8]) -> Result<LibraryArtifact, LibraryArtifactError> {
    let envelope = serde_json::from_slice::<LibraryEnvelope>(bytes)
        .map_err(|error| LibraryArtifactError::InvalidJson(error.to_string()))?;
    if envelope.format != LIBRARY_ARTIFACT_FORMAT {
        return Err(LibraryArtifactError::FormatMismatch {
            found: envelope.format,
        });
    }
    if envelope.version != LIBRARY_ARTIFACT_VERSION {
        return Err(LibraryArtifactError::VersionMismatch {
            found: envelope.version,
        });
    }
    if envelope.language_version != LOOM_LANGUAGE_VERSION {
        return Err(LibraryArtifactError::LanguageVersionMismatch {
            found: envelope.language_version,
        });
    }
    validate_graph(&envelope)?;
    let program = decode_interpreted_artifact(envelope.checked_mir.as_bytes())
        .map_err(|error| LibraryArtifactError::InvalidMir(error.to_string()))?;
    Ok(LibraryArtifact {
        root_package: envelope.root_package,
        packages: envelope.packages,
        sources: envelope.sources,
        interfaces: envelope.public_interfaces,
        program,
    })
}

fn validate_graph(envelope: &LibraryEnvelope) -> Result<(), LibraryArtifactError> {
    let mut packages = BTreeSet::new();
    for package in &envelope.packages {
        if package.id.language() != envelope.language_version {
            return Err(LibraryArtifactError::InvalidGraph(format!(
                "package `{}` uses language `{}`",
                package.id,
                package.id.language()
            )));
        }
        if !packages.insert(package.id.clone()) {
            return Err(LibraryArtifactError::InvalidGraph(format!(
                "duplicate package `{}`",
                package.id
            )));
        }
        let mut aliases = BTreeSet::new();
        for dependency in &package.dependencies {
            if !aliases.insert(&dependency.alias) {
                return Err(LibraryArtifactError::InvalidGraph(format!(
                    "package `{}` repeats dependency alias `{}`",
                    package.id, dependency.alias
                )));
            }
        }
    }
    if !packages.contains(&envelope.root_package) {
        return Err(LibraryArtifactError::InvalidGraph(format!(
            "root package `{}` is absent",
            envelope.root_package
        )));
    }
    for package in &envelope.packages {
        for dependency in &package.dependencies {
            if !packages.contains(&dependency.package) {
                return Err(LibraryArtifactError::InvalidGraph(format!(
                    "package `{}` depends on absent package `{}`",
                    package.id, dependency.package
                )));
            }
        }
    }
    let mut source_paths = BTreeSet::new();
    for source in &envelope.sources {
        if !packages.contains(&source.package) {
            return Err(LibraryArtifactError::InvalidGraph(format!(
                "source `{}` belongs to absent package `{}`",
                source.path, source.package
            )));
        }
        if !portable_relative_path(&source.path) {
            return Err(LibraryArtifactError::InvalidGraph(format!(
                "source path `{}` is not portable",
                source.path
            )));
        }
        if !source_paths.insert((source.package.clone(), source.path.clone())) {
            return Err(LibraryArtifactError::InvalidGraph(format!(
                "duplicate source `{}` in package `{}`",
                source.path, source.package
            )));
        }
    }
    let mut interface_modules = BTreeSet::new();
    for interface in &envelope.public_interfaces {
        if !interface_modules.insert(&interface.module) {
            return Err(LibraryArtifactError::InvalidGraph(format!(
                "duplicate public interface `{}`",
                interface.module
            )));
        }
    }
    Ok(())
}

fn portable_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.starts_with('\\')
        && path.split('/').all(|component| {
            !component.is_empty()
                && component != "."
                && component != ".."
                && !component.contains('\\')
        })
}

fn package_relative_path(stable_path: &str, package: &PackageId) -> String {
    stable_path
        .strip_prefix(&format!("deps/{package}/"))
        .unwrap_or(stable_path)
        .to_owned()
}
