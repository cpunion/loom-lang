//! Portable source package artifacts consumed by manifest dependencies.
//!
//! A `.loomlib` is deliberately not a native ABI. It carries the resolved
//! package graph, canonical public-interface fingerprints, compiler-private
//! source payloads. The consuming compiler checks those sources normally, so
//! generic instantiation, proof search, and the compiler-owned `std` package
//! always use the consumer toolchain. Only the public interface is part of the
//! user-facing dependency surface.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use loom_core::{FileId, LOOM_LANGUAGE_VERSION, PackageId};
use loom_syntax::parse_with_file;
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};

use crate::incremental::embedded_module_interfaces;
use crate::{ModuleInterface, ProjectGraph, SourceMap};

/// Maximum encoded size accepted for one portable source package artifact.
pub const LIBRARY_ARTIFACT_MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_LIBRARY_PACKAGES: usize = 4096;
const MAX_LIBRARY_SOURCES: usize = 4096;
const MAX_LIBRARY_INTERFACES: usize = 4096;
const MAX_LIBRARY_SOURCE_BYTES: usize = 16 * 1024 * 1024;

/// Wire format name for portable package artifacts.
pub const LIBRARY_ARTIFACT_FORMAT: &str = "loom-library";
/// Version of the portable source package envelope.
pub const LIBRARY_ARTIFACT_VERSION: u32 = 2;

/// A decoded `.loomlib` which has crossed structural package validation.
#[derive(Clone, Debug)]
pub struct LibraryArtifact {
    pub(crate) root_package: PackageId,
    pub(crate) packages: Vec<LibraryPackage>,
    pub(crate) sources: Vec<LibrarySource>,
    interfaces: Vec<ModuleInterface>,
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
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LibraryHeader {
    format: String,
    version: u32,
    language_version: String,
}

/// A fail-closed portable library codec error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LibraryArtifactError {
    InvalidJson(String),
    FormatMismatch { found: String },
    VersionMismatch { found: u32 },
    LanguageVersionMismatch { found: String },
    InvalidGraph(String),
}

impl LibraryArtifactError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::LanguageVersionMismatch { .. } => "ArtifactLanguageVersionMismatch",
            Self::FormatMismatch { .. } | Self::VersionMismatch { .. } => {
                "LibraryArtifactVersionMismatch"
            }
            Self::InvalidJson(_) | Self::InvalidGraph(_) => "InvalidLibraryArtifact",
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
        }
    }
}

impl std::error::Error for LibraryArtifactError {}

/// Encodes one resolved package graph as a deterministic portable library.
///
/// # Errors
///
/// Returns an error if the graph has no manifest root, source provenance is
/// incomplete or inconsistent, or the compiler-owned `std` package leaked
/// into the payload.
pub fn encode_library_artifact(
    project: &ProjectGraph,
    sources: &SourceMap,
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
    let mut embedded_sources = Vec::new();
    for source in sources.documents() {
        let package = source.package().cloned().ok_or_else(|| {
            LibraryArtifactError::InvalidGraph(format!(
                "source `{}` has no package provenance",
                source.relative_path()
            ))
        })?;
        if source.is_authoritative_compiler_std() {
            continue;
        }
        if source.is_compiler_std() || package.name() == crate::stdlib::STD_PACKAGE_NAME {
            return Err(LibraryArtifactError::InvalidGraph(format!(
                "source `{}` has inconsistent compiler-owned `std` provenance: origin {:?}, package `{package}`",
                source.relative_path(),
                source.origin(),
            )));
        }
        let text = source.text().ok_or_else(|| {
            LibraryArtifactError::InvalidGraph(format!(
                "source `{}` is not valid UTF-8",
                source.relative_path()
            ))
        })?;
        embedded_sources.push(LibrarySource {
            path: if source.is_root_package() {
                source.relative_path().to_owned()
            } else {
                package_relative_path(source.relative_path(), &package)
            },
            package,
            text: text.to_owned(),
        });
    }
    embedded_sources.sort_by(|left, right| {
        left.package
            .cmp(&right.package)
            .then(left.path.cmp(&right.path))
    });
    let interfaces = interfaces_from_sources(&embedded_sources)?;
    let envelope = LibraryEnvelope {
        format: LIBRARY_ARTIFACT_FORMAT.to_owned(),
        version: LIBRARY_ARTIFACT_VERSION,
        language_version: project.language_version().to_owned(),
        root_package,
        packages,
        public_interfaces: interfaces,
        sources: embedded_sources,
    };
    validate_graph(&envelope)?;
    let bytes = serde_json::to_vec(&envelope)
        .map_err(|error| LibraryArtifactError::InvalidJson(error.to_string()))?;
    if bytes.len() > LIBRARY_ARTIFACT_MAX_BYTES {
        return Err(LibraryArtifactError::InvalidJson(format!(
            ".loomlib exceeds the {LIBRARY_ARTIFACT_MAX_BYTES}-byte limit"
        )));
    }
    Ok(bytes)
}

/// Decodes and structurally validates a portable source library.
///
/// # Errors
///
/// Rejects incompatible formats/languages, malformed package graphs, private
/// path escapes, duplicate identities, and reserved compiler-owned names. The
/// normal frontend checks the embedded source after dependency resolution.
pub fn decode_library_artifact(bytes: &[u8]) -> Result<LibraryArtifact, LibraryArtifactError> {
    if bytes.len() > LIBRARY_ARTIFACT_MAX_BYTES {
        return Err(LibraryArtifactError::InvalidJson(format!(
            ".loomlib exceeds the {LIBRARY_ARTIFACT_MAX_BYTES}-byte limit"
        )));
    }
    let header = serde_json::from_slice::<LibraryHeader>(bytes)
        .map_err(|error| LibraryArtifactError::InvalidJson(error.to_string()))?;
    if header.format != LIBRARY_ARTIFACT_FORMAT {
        return Err(LibraryArtifactError::FormatMismatch {
            found: header.format,
        });
    }
    if header.version != LIBRARY_ARTIFACT_VERSION {
        return Err(LibraryArtifactError::VersionMismatch {
            found: header.version,
        });
    }
    if header.language_version != LOOM_LANGUAGE_VERSION {
        return Err(LibraryArtifactError::LanguageVersionMismatch {
            found: header.language_version,
        });
    }
    let envelope = serde_json::from_slice::<LibraryEnvelope>(bytes)
        .map_err(|error| LibraryArtifactError::InvalidJson(error.to_string()))?;
    validate_graph(&envelope)?;
    let interfaces = interfaces_from_sources(&envelope.sources)?;
    if interfaces != envelope.public_interfaces {
        return Err(LibraryArtifactError::InvalidGraph(
            "public interfaces do not match embedded source".to_owned(),
        ));
    }
    Ok(LibraryArtifact {
        root_package: envelope.root_package,
        packages: envelope.packages,
        sources: envelope.sources,
        interfaces: envelope.public_interfaces,
    })
}

fn validate_graph(envelope: &LibraryEnvelope) -> Result<(), LibraryArtifactError> {
    validate_envelope_counts(envelope)?;
    let packages = validate_packages(envelope)?;
    validate_dependencies(envelope, &packages)?;
    validate_closed_acyclic_graph(&envelope.root_package, &packages)?;
    validate_sources(envelope, &packages)?;
    validate_interfaces(envelope)
}

fn validate_envelope_counts(envelope: &LibraryEnvelope) -> Result<(), LibraryArtifactError> {
    if envelope.packages.len() > MAX_LIBRARY_PACKAGES {
        return Err(LibraryArtifactError::InvalidGraph(format!(
            "package count exceeds {MAX_LIBRARY_PACKAGES}"
        )));
    }
    if envelope.sources.len() > MAX_LIBRARY_SOURCES {
        return Err(LibraryArtifactError::InvalidGraph(format!(
            "source count exceeds {MAX_LIBRARY_SOURCES}"
        )));
    }
    if envelope.public_interfaces.len() > MAX_LIBRARY_INTERFACES {
        return Err(LibraryArtifactError::InvalidGraph(format!(
            "public interface count exceeds {MAX_LIBRARY_INTERFACES}"
        )));
    }
    Ok(())
}

fn validate_packages(
    envelope: &LibraryEnvelope,
) -> Result<BTreeMap<PackageId, &LibraryPackage>, LibraryArtifactError> {
    let mut package_by_id = BTreeMap::new();
    for package in &envelope.packages {
        if !valid_package_name(package.id.name()) {
            return Err(LibraryArtifactError::InvalidGraph(format!(
                "package name `{}` must match [a-z][a-z0-9_-]*",
                package.id.name()
            )));
        }
        if package.id.name() == crate::stdlib::STD_PACKAGE_NAME {
            return Err(LibraryArtifactError::InvalidGraph(
                "portable libraries cannot define the reserved package `std`".to_owned(),
            ));
        }
        Version::parse(package.id.version()).map_err(|error| {
            LibraryArtifactError::InvalidGraph(format!(
                "package `{}` has an invalid semantic version: {error}",
                package.id
            ))
        })?;
        if package.id.language() != envelope.language_version {
            return Err(LibraryArtifactError::InvalidGraph(format!(
                "package `{}` uses language `{}`",
                package.id,
                package.id.language()
            )));
        }
        if package_by_id.insert(package.id.clone(), package).is_some() {
            return Err(LibraryArtifactError::InvalidGraph(format!(
                "duplicate package `{}`",
                package.id
            )));
        }
        let mut aliases = BTreeSet::new();
        for dependency in &package.dependencies {
            if !valid_package_name(&dependency.alias) {
                return Err(LibraryArtifactError::InvalidGraph(format!(
                    "dependency alias `{}` must match [a-z][a-z0-9_-]*",
                    dependency.alias
                )));
            }
            if dependency.alias == crate::stdlib::STD_PACKAGE_NAME {
                return Err(LibraryArtifactError::InvalidGraph(
                    "portable libraries cannot define the reserved dependency alias `std`"
                        .to_owned(),
                ));
            }
            if !aliases.insert(&dependency.alias) {
                return Err(LibraryArtifactError::InvalidGraph(format!(
                    "package `{}` repeats dependency alias `{}`",
                    package.id, dependency.alias
                )));
            }
        }
    }
    if !package_by_id.contains_key(&envelope.root_package) {
        return Err(LibraryArtifactError::InvalidGraph(format!(
            "root package `{}` is absent",
            envelope.root_package
        )));
    }
    Ok(package_by_id)
}

fn validate_dependencies(
    envelope: &LibraryEnvelope,
    packages: &BTreeMap<PackageId, &LibraryPackage>,
) -> Result<(), LibraryArtifactError> {
    for package in &envelope.packages {
        for dependency in &package.dependencies {
            if !packages.contains_key(&dependency.package) {
                return Err(LibraryArtifactError::InvalidGraph(format!(
                    "package `{}` depends on absent package `{}`",
                    package.id, dependency.package
                )));
            }
            if let Some(requirement) = &dependency.requirement {
                let requirement = VersionReq::parse(requirement).map_err(|error| {
                    LibraryArtifactError::InvalidGraph(format!(
                        "package `{}` has invalid requirement `{requirement}` for `{}`: {error}",
                        package.id, dependency.alias
                    ))
                })?;
                let resolved = Version::parse(dependency.package.version())
                    .expect("package versions were validated above");
                if !requirement.matches(&resolved) {
                    return Err(LibraryArtifactError::InvalidGraph(format!(
                        "package `{}` requires `{}` as `{requirement}` but resolves `{}`",
                        package.id, dependency.alias, dependency.package
                    )));
                }
            }
        }
    }
    Ok(())
}

fn validate_sources(
    envelope: &LibraryEnvelope,
    packages: &BTreeMap<PackageId, &LibraryPackage>,
) -> Result<(), LibraryArtifactError> {
    let mut source_paths = BTreeSet::new();
    for source in &envelope.sources {
        if source.text.len() > MAX_LIBRARY_SOURCE_BYTES {
            return Err(LibraryArtifactError::InvalidGraph(format!(
                "source `{}` exceeds {MAX_LIBRARY_SOURCE_BYTES} bytes",
                source.path
            )));
        }
        if !packages.contains_key(&source.package) {
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
    Ok(())
}

fn validate_interfaces(envelope: &LibraryEnvelope) -> Result<(), LibraryArtifactError> {
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

fn interfaces_from_sources(
    sources: &[LibrarySource],
) -> Result<Vec<ModuleInterface>, LibraryArtifactError> {
    if sources.len() > MAX_LIBRARY_SOURCES {
        return Err(LibraryArtifactError::InvalidGraph(format!(
            "source count exceeds {MAX_LIBRARY_SOURCES}"
        )));
    }
    let mut parses = Vec::with_capacity(sources.len());
    for (index, source) in sources.iter().enumerate() {
        let file = FileId(u32::try_from(index).expect("library source count is bounded"));
        let parse = parse_with_file(file, &source.text);
        if let Some(diagnostic) = parse.diagnostics().first() {
            return Err(LibraryArtifactError::InvalidGraph(format!(
                "source `{}` does not parse: {}",
                source.path, diagnostic.message
            )));
        }
        parses.push(parse);
    }
    Ok(embedded_module_interfaces(
        sources
            .iter()
            .zip(&parses)
            .enumerate()
            .map(|(index, (source, parse))| {
                (
                    FileId(u32::try_from(index).expect("library source count is bounded")),
                    &source.package,
                    source.path.as_str(),
                    parse,
                )
            }),
    ))
}

fn validate_closed_acyclic_graph(
    root: &PackageId,
    packages: &BTreeMap<PackageId, &LibraryPackage>,
) -> Result<(), LibraryArtifactError> {
    let mut reachable = BTreeSet::new();
    let mut pending = vec![root.clone()];
    while let Some(package) = pending.pop() {
        if !reachable.insert(package.clone()) {
            continue;
        }
        pending.extend(
            packages[&package]
                .dependencies
                .iter()
                .map(|dependency| dependency.package.clone()),
        );
    }
    if reachable.len() != packages.len() {
        let unreachable = packages
            .keys()
            .find(|package| !reachable.contains(*package))
            .expect("package counts differ");
        return Err(LibraryArtifactError::InvalidGraph(format!(
            "package `{unreachable}` is not reachable from root `{root}`"
        )));
    }

    let mut incoming = packages
        .keys()
        .cloned()
        .map(|package| (package, 0_usize))
        .collect::<BTreeMap<_, _>>();
    for package in packages.values() {
        for dependency in &package.dependencies {
            *incoming
                .get_mut(&dependency.package)
                .expect("dependency presence was validated") += 1;
        }
    }
    let mut ready = incoming
        .iter()
        .filter_map(|(package, count)| (*count == 0).then_some(package.clone()))
        .collect::<Vec<_>>();
    let mut visited = 0_usize;
    while let Some(package) = ready.pop() {
        visited += 1;
        for dependency in &packages[&package].dependencies {
            let count = incoming
                .get_mut(&dependency.package)
                .expect("dependency presence was validated");
            *count -= 1;
            if *count == 0 {
                ready.push(dependency.package.clone());
            }
        }
    }
    if visited != packages.len() {
        return Err(LibraryArtifactError::InvalidGraph(
            "package dependency graph contains a cycle".to_owned(),
        ));
    }
    Ok(())
}

fn valid_package_name(name: &str) -> bool {
    name.bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-".contains(&byte))
}

fn portable_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.starts_with('\\')
        && path.split('/').all(portable_path_component)
}

fn portable_path_component(component: &str) -> bool {
    if component.is_empty()
        || component == "."
        || component == ".."
        || component.ends_with(['.', ' '])
        || component
            .chars()
            .any(|character| character.is_control() || r#"\/:*?"<>|"#.contains(character))
    {
        return false;
    }
    let stem = component
        .split_once('.')
        .map_or(component, |(stem, _)| stem)
        .to_ascii_uppercase();
    !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        && !(stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0')
}

fn package_relative_path(stable_path: &str, package: &PackageId) -> String {
    stable_path
        .strip_prefix(&format!("deps/{package}/"))
        .unwrap_or(stable_path)
        .to_owned()
}
