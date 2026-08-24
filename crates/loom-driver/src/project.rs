//! Manifest, package-dependency, target, and source-root resolution.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

use semver::{Version, VersionReq};
use serde::Deserialize;

use crate::DriverError;
use crate::source::{
    discover_loom_files, has_loom_extension, is_ignored_relative, normalize_absolute, relative_key,
};

pub const MANIFEST_FILE: &str = "loom.toml";
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PackageId {
    name: String,
    version: String,
}

impl PackageId {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }
}

impl fmt::Display for PackageId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}@{}", self.name, self.version)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageDependency {
    alias: String,
    requirement: Option<String>,
    package: PackageId,
}

impl PackageDependency {
    #[must_use]
    pub fn alias(&self) -> &str {
        &self.alias
    }

    #[must_use]
    pub fn requirement(&self) -> Option<&str> {
        self.requirement.as_deref()
    }

    #[must_use]
    pub const fn package(&self) -> &PackageId {
        &self.package
    }
}

#[derive(Clone, Debug)]
struct SourceRoot {
    declared: String,
    absolute: PathBuf,
    is_file: bool,
}

#[derive(Clone, Debug)]
pub struct Package {
    id: PackageId,
    root: PathBuf,
    manifest: PathBuf,
    source_roots: Vec<SourceRoot>,
    dependencies: Vec<PackageDependency>,
    targets: Vec<Target>,
}

impl Package {
    #[must_use]
    pub const fn id(&self) -> &PackageId {
        &self.id
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn manifest(&self) -> &Path {
        &self.manifest
    }

    pub fn source_roots(&self) -> impl Iterator<Item = &str> {
        self.source_roots
            .iter()
            .map(|source| source.declared.as_str())
    }

    #[must_use]
    pub fn dependencies(&self) -> &[PackageDependency] {
        &self.dependencies
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetKind {
    Bin,
    Test,
}

impl TargetKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bin => "bin",
            Self::Test => "test",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Target {
    name: String,
    kind: TargetKind,
    entry: Option<String>,
}

impl Target {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn kind(&self) -> TargetKind {
        self.kind
    }

    #[must_use]
    pub fn entry(&self) -> Option<&str> {
        self.entry.as_deref()
    }
}

#[derive(Clone, Debug)]
enum ProjectKind {
    Legacy { input: PathBuf },
    Manifest { root_package: PackageId },
}

/// Fully resolved package dependency and target graph.
#[derive(Clone, Debug)]
pub struct ProjectGraph {
    root: PathBuf,
    manifest: Option<PathBuf>,
    kind: ProjectKind,
    packages: BTreeMap<PackageId, Package>,
    targets: Vec<Target>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProjectSource {
    pub absolute: PathBuf,
    pub stable_path: String,
}

impl ProjectGraph {
    /// Resolves a manifest project or a legacy directory/single-file input.
    ///
    /// # Errors
    ///
    /// Returns a project error for unreadable inputs, invalid manifests,
    /// dependency cycles, or source roots which leave their package.
    pub fn load(input: impl AsRef<Path>) -> Result<Self, DriverError> {
        let original = input.as_ref();
        let canonical = fs::canonicalize(original).map_err(|source| DriverError::Io {
            path: original.to_path_buf(),
            source,
        })?;
        let metadata = fs::metadata(&canonical).map_err(|source| DriverError::Io {
            path: canonical.clone(),
            source,
        })?;

        let manifest = if metadata.is_dir() {
            let candidate = canonical.join(MANIFEST_FILE);
            candidate.is_file().then_some(candidate)
        } else if metadata.is_file()
            && canonical
                .file_name()
                .is_some_and(|name| name == MANIFEST_FILE)
        {
            Some(canonical.clone())
        } else {
            None
        };

        if let Some(manifest) = manifest {
            return Self::load_manifest(&manifest);
        }
        if metadata.is_dir() || metadata.is_file() && has_loom_extension(&canonical) {
            let root = if metadata.is_dir() {
                canonical.clone()
            } else {
                canonical
                    .parent()
                    .ok_or_else(|| DriverError::InvalidRoot(canonical.clone()))?
                    .to_path_buf()
            };
            return Ok(Self {
                root,
                manifest: None,
                kind: ProjectKind::Legacy { input: canonical },
                packages: BTreeMap::new(),
                targets: Vec::new(),
            });
        }
        Err(DriverError::InvalidRoot(canonical))
    }

    fn load_manifest(manifest: &Path) -> Result<Self, DriverError> {
        let mut resolver = Resolver::default();
        let root_package = resolver.resolve(manifest, &mut Vec::new())?;
        let package = resolver
            .packages
            .get(&root_package)
            .expect("resolved root package is present");
        Ok(Self {
            root: package.root.clone(),
            manifest: Some(package.manifest.clone()),
            kind: ProjectKind::Manifest {
                root_package: root_package.clone(),
            },
            targets: package.targets.clone(),
            packages: resolver.packages,
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn manifest(&self) -> Option<&Path> {
        self.manifest.as_deref()
    }

    #[must_use]
    pub fn root_package(&self) -> Option<&Package> {
        let ProjectKind::Manifest { root_package } = &self.kind else {
            return None;
        };
        self.packages.get(root_package)
    }

    pub fn packages(&self) -> impl Iterator<Item = &Package> {
        self.packages.values()
    }

    #[must_use]
    pub fn targets(&self) -> &[Target] {
        &self.targets
    }

    #[must_use]
    pub fn target(&self, name: &str) -> Option<&Target> {
        self.targets.iter().find(|target| target.name == name)
    }

    #[must_use]
    pub fn cache_root(&self) -> PathBuf {
        self.root.join("target/loom/cache/v1")
    }

    pub(crate) fn source_files(&self) -> Result<Vec<ProjectSource>, DriverError> {
        match &self.kind {
            ProjectKind::Legacy { input } => legacy_sources(&self.root, input),
            ProjectKind::Manifest { root_package } => {
                let mut sources = BTreeMap::<PathBuf, String>::new();
                for package in self.packages.values() {
                    let is_root = &package.id == root_package;
                    for source_root in &package.source_roots {
                        for path in discover_package_source(source_root, &package.root)? {
                            let relative = relative_key(&package.root, &path)
                                .ok_or_else(|| DriverError::NonUtf8Path(path.clone()))?;
                            let stable_path = if is_root {
                                relative
                            } else {
                                format!("deps/{}/{relative}", package.id)
                            };
                            if let Some(previous) =
                                sources.insert(path.clone(), stable_path.clone())
                                && previous != stable_path
                            {
                                return Err(manifest_error(
                                    &package.manifest,
                                    format!(
                                        "source `{}` is selected through both `{previous}` and `{stable_path}`",
                                        path.display()
                                    ),
                                ));
                            }
                        }
                    }
                }
                let mut result = sources
                    .into_iter()
                    .map(|(absolute, stable_path)| ProjectSource {
                        absolute,
                        stable_path,
                    })
                    .collect::<Vec<_>>();
                result.sort_by(|left, right| left.stable_path.cmp(&right.stable_path));
                Ok(result)
            }
        }
    }

    pub(crate) fn overlay_stable_path(&self, path: &Path) -> Option<String> {
        if !has_loom_extension(path) || is_ignored_relative(&self.root, path) {
            return None;
        }
        match &self.kind {
            ProjectKind::Legacy { input } => {
                let metadata = fs::metadata(input).ok()?;
                if metadata.is_file() && normalize_absolute(input) != normalize_absolute(path) {
                    return None;
                }
                relative_key(&self.root, path)
            }
            ProjectKind::Manifest { root_package } => {
                let package = self.packages.get(root_package)?;
                let normalized = normalize_absolute(path);
                let selected = package.source_roots.iter().any(|source| {
                    if source.is_file {
                        normalize_absolute(&source.absolute) == normalized
                    } else {
                        normalized.strip_prefix(&source.absolute).is_ok()
                    }
                });
                selected
                    .then(|| relative_key(&package.root, &normalized))
                    .flatten()
            }
        }
    }

    pub(crate) fn identity_fields(&self) -> Vec<String> {
        match &self.kind {
            ProjectKind::Legacy { .. } => vec!["project:legacy".to_owned()],
            ProjectKind::Manifest { root_package } => {
                let mut fields = vec![
                    format!("manifest-schema:{MANIFEST_SCHEMA_VERSION}"),
                    format!("root-package:{root_package}"),
                ];
                for package in self.packages.values() {
                    fields.push(format!("package:{}", package.id));
                    for source in &package.source_roots {
                        fields.push(format!("source-root:{}:{}", package.id, source.declared));
                    }
                    for dependency in &package.dependencies {
                        fields.push(format!(
                            "dependency:{}:{}:{}:{}",
                            package.id,
                            dependency.alias,
                            dependency.requirement.as_deref().unwrap_or("*"),
                            dependency.package
                        ));
                    }
                }
                for target in &self.targets {
                    fields.push(format!(
                        "target:{}:{}:{}",
                        target.name,
                        target.kind.as_str(),
                        target.entry.as_deref().unwrap_or("")
                    ));
                }
                fields
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    schema: u32,
    package: RawPackage,
    #[serde(default)]
    dependencies: BTreeMap<String, RawDependency>,
    #[serde(default, rename = "target")]
    targets: Vec<RawTarget>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPackage {
    name: String,
    version: String,
    #[serde(default = "default_sources")]
    sources: Vec<String>,
}

fn default_sources() -> Vec<String> {
    vec!["src".to_owned()]
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDependency {
    path: String,
    version: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTarget {
    name: String,
    kind: RawTargetKind,
    entry: Option<String>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum RawTargetKind {
    Bin,
    Test,
}

#[derive(Default)]
struct Resolver {
    packages: BTreeMap<PackageId, Package>,
    by_manifest: BTreeMap<PathBuf, PackageId>,
}

impl Resolver {
    fn resolve(
        &mut self,
        manifest: &Path,
        stack: &mut Vec<PathBuf>,
    ) -> Result<PackageId, DriverError> {
        let manifest = canonical_manifest(manifest)?;
        if let Some(package) = self.by_manifest.get(&manifest) {
            return Ok(package.clone());
        }
        if let Some(start) = stack.iter().position(|candidate| candidate == &manifest) {
            let mut cycle = stack[start..]
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>();
            cycle.push(manifest.display().to_string());
            return Err(manifest_error(
                &manifest,
                format!("package dependency cycle: {}", cycle.join(" -> ")),
            ));
        }

        let text = fs::read_to_string(&manifest).map_err(|source| DriverError::Io {
            path: manifest.clone(),
            source,
        })?;
        let raw: RawManifest = toml::from_str(&text)
            .map_err(|error| manifest_error(&manifest, format!("invalid TOML: {error}")))?;
        if raw.schema != MANIFEST_SCHEMA_VERSION {
            return Err(manifest_error(
                &manifest,
                format!(
                    "manifest schema {} is incompatible with supported schema {MANIFEST_SCHEMA_VERSION}",
                    raw.schema
                ),
            ));
        }
        validate_name("package", &raw.package.name, &manifest)?;
        Version::parse(&raw.package.version).map_err(|error| {
            manifest_error(
                &manifest,
                format!(
                    "package version `{}` is not SemVer: {error}",
                    raw.package.version
                ),
            )
        })?;
        let id = PackageId {
            name: raw.package.name,
            version: raw.package.version,
        };
        let root = manifest
            .parent()
            .expect("canonical manifest has a parent")
            .to_path_buf();
        let source_roots = resolve_source_roots(&root, &manifest, raw.package.sources)?;
        let targets = resolve_targets(&manifest, raw.targets)?;

        stack.push(manifest.clone());
        let dependencies = self.resolve_dependencies(&manifest, &root, raw.dependencies, stack)?;
        stack.pop();

        if let Some(previous) = self.packages.get(&id) {
            return Err(manifest_error(
                &manifest,
                format!(
                    "package identity `{id}` is also provided by {}",
                    previous.manifest.display()
                ),
            ));
        }
        self.by_manifest.insert(manifest.clone(), id.clone());
        self.packages.insert(
            id.clone(),
            Package {
                id: id.clone(),
                root,
                manifest,
                source_roots,
                dependencies,
                targets,
            },
        );
        Ok(id)
    }

    fn resolve_dependencies(
        &mut self,
        manifest: &Path,
        root: &Path,
        raw_dependencies: BTreeMap<String, RawDependency>,
        stack: &mut Vec<PathBuf>,
    ) -> Result<Vec<PackageDependency>, DriverError> {
        let mut dependencies = Vec::with_capacity(raw_dependencies.len());
        for (alias, dependency) in raw_dependencies {
            validate_name("dependency alias", &alias, manifest)?;
            if dependency.path.is_empty() {
                return Err(manifest_error(
                    manifest,
                    format!("dependency `{alias}` has an empty path"),
                ));
            }
            let dependency_path = Path::new(&dependency.path);
            if dependency_path.is_absolute() {
                return Err(manifest_error(
                    manifest,
                    format!("dependency `{alias}` path must be relative"),
                ));
            }
            let child_manifest = dependency_manifest(&root.join(dependency_path))?;
            let child = self.resolve(&child_manifest, stack)?;
            if let Some(requirement) = &dependency.version {
                let parsed = VersionReq::parse(requirement).map_err(|error| {
                    manifest_error(
                        manifest,
                        format!(
                            "dependency `{alias}` version requirement `{requirement}` is invalid: {error}"
                        ),
                    )
                })?;
                let actual =
                    Version::parse(child.version()).expect("package version was validated");
                if !parsed.matches(&actual) {
                    return Err(manifest_error(
                        manifest,
                        format!(
                            "dependency `{alias}` requires `{requirement}`, but `{child}` was resolved"
                        ),
                    ));
                }
            }
            dependencies.push(PackageDependency {
                alias,
                requirement: dependency.version,
                package: child,
            });
        }
        Ok(dependencies)
    }
}

fn canonical_manifest(input: &Path) -> Result<PathBuf, DriverError> {
    let canonical = fs::canonicalize(input).map_err(|source| DriverError::Io {
        path: input.to_path_buf(),
        source,
    })?;
    let metadata = fs::metadata(&canonical).map_err(|source| DriverError::Io {
        path: canonical.clone(),
        source,
    })?;
    if metadata.is_file()
        && canonical
            .file_name()
            .is_some_and(|name| name == MANIFEST_FILE)
    {
        Ok(canonical)
    } else {
        Err(manifest_error(
            &canonical,
            format!("package path must contain {MANIFEST_FILE}"),
        ))
    }
}

fn dependency_manifest(input: &Path) -> Result<PathBuf, DriverError> {
    let canonical = fs::canonicalize(input).map_err(|source| DriverError::Io {
        path: input.to_path_buf(),
        source,
    })?;
    if canonical.is_dir() {
        canonical_manifest(&canonical.join(MANIFEST_FILE))
    } else {
        canonical_manifest(&canonical)
    }
}

fn resolve_source_roots(
    package_root: &Path,
    manifest: &Path,
    declared_roots: Vec<String>,
) -> Result<Vec<SourceRoot>, DriverError> {
    if declared_roots.is_empty() {
        return Err(manifest_error(manifest, "package.sources cannot be empty"));
    }
    let mut seen = BTreeSet::new();
    let mut roots = Vec::new();
    for declared in declared_roots {
        let relative = manifest_relative_path(manifest, "source root", &declared, false)?;
        let absolute =
            fs::canonicalize(package_root.join(&relative)).map_err(|source| DriverError::Io {
                path: package_root.join(&relative),
                source,
            })?;
        if absolute.strip_prefix(package_root).is_err() {
            return Err(manifest_error(
                manifest,
                format!("source root `{declared}` leaves its package"),
            ));
        }
        let metadata = fs::metadata(&absolute).map_err(|source| DriverError::Io {
            path: absolute.clone(),
            source,
        })?;
        let is_file = metadata.is_file();
        if !(metadata.is_dir() || is_file && has_loom_extension(&absolute)) {
            return Err(manifest_error(
                manifest,
                format!("source root `{declared}` must be a directory or .loom file"),
            ));
        }
        let normalized = relative_key(package_root, &absolute)
            .ok_or_else(|| DriverError::NonUtf8Path(absolute.clone()))?;
        let normalized = if normalized.is_empty() {
            ".".to_owned()
        } else {
            normalized
        };
        if seen.insert(absolute.clone()) {
            roots.push(SourceRoot {
                declared: normalized,
                absolute,
                is_file,
            });
        }
    }
    roots.sort_by(|left, right| left.declared.cmp(&right.declared));
    Ok(roots)
}

fn resolve_targets(
    manifest: &Path,
    raw_targets: Vec<RawTarget>,
) -> Result<Vec<Target>, DriverError> {
    let mut names = BTreeSet::new();
    let mut targets = Vec::new();
    for raw in raw_targets {
        validate_name("target", &raw.name, manifest)?;
        if !names.insert(raw.name.clone()) {
            return Err(manifest_error(
                manifest,
                format!("duplicate target `{}`", raw.name),
            ));
        }
        let (kind, entry) = match raw.kind {
            RawTargetKind::Bin => {
                let entry = raw.entry.unwrap_or_else(|| "main".to_owned());
                if entry.is_empty() {
                    return Err(manifest_error(
                        manifest,
                        format!("binary target `{}` has an empty entry", raw.name),
                    ));
                }
                (TargetKind::Bin, Some(entry))
            }
            RawTargetKind::Test => {
                if raw.entry.is_some() {
                    return Err(manifest_error(
                        manifest,
                        format!("test target `{}` cannot declare entry", raw.name),
                    ));
                }
                (TargetKind::Test, None)
            }
        };
        targets.push(Target {
            name: raw.name,
            kind,
            entry,
        });
    }
    targets.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(targets)
}

fn validate_name(kind: &str, name: &str, manifest: &Path) -> Result<(), DriverError> {
    let valid = name
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase())
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-".contains(&byte)
        });
    if valid {
        Ok(())
    } else {
        Err(manifest_error(
            manifest,
            format!("{kind} name `{name}` must match [a-z][a-z0-9_-]*"),
        ))
    }
}

fn manifest_relative_path(
    manifest: &Path,
    kind: &str,
    value: &str,
    allow_parent: bool,
) -> Result<PathBuf, DriverError> {
    let path = Path::new(value);
    if value.is_empty() || path.is_absolute() {
        return Err(manifest_error(
            manifest,
            format!("{kind} `{value}` must be a non-empty relative path"),
        ));
    }
    if !allow_parent
        && path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(manifest_error(
            manifest,
            format!("{kind} `{value}` cannot contain `..`"),
        ));
    }
    Ok(path.to_path_buf())
}

fn discover_package_source(
    source: &SourceRoot,
    package_root: &Path,
) -> Result<Vec<PathBuf>, DriverError> {
    if source.is_file {
        return Ok(vec![source.absolute.clone()]);
    }
    let mut pending = vec![source.absolute.clone()];
    let mut files = BTreeSet::new();
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory).map_err(|source| DriverError::Io {
            path: directory.clone(),
            source,
        })?;
        let mut entries =
            entries
                .collect::<Result<Vec<_>, _>>()
                .map_err(|source| DriverError::Io {
                    path: directory.clone(),
                    source,
                })?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries.into_iter().rev() {
            let path = entry.path();
            let file_type = entry.file_type().map_err(|source| DriverError::Io {
                path: path.clone(),
                source,
            })?;
            if file_type.is_dir() {
                if is_ignored_relative(package_root, &path)
                    || path != *package_root && path.join(MANIFEST_FILE).is_file()
                {
                    continue;
                }
                pending.push(path);
            } else if file_type.is_file() && has_loom_extension(&path) {
                files.insert(normalize_absolute(&path));
            }
        }
    }
    Ok(files.into_iter().collect())
}

fn legacy_sources(root: &Path, input: &Path) -> Result<Vec<ProjectSource>, DriverError> {
    let paths = if input.is_file() {
        vec![input.to_path_buf()]
    } else {
        discover_loom_files(root)?
    };
    paths
        .into_iter()
        .map(|absolute| {
            let stable_path = relative_key(root, &absolute)
                .ok_or_else(|| DriverError::NonUtf8Path(absolute.clone()))?;
            Ok(ProjectSource {
                absolute,
                stable_path,
            })
        })
        .collect()
}

fn manifest_error(path: &Path, message: impl Into<String>) -> DriverError {
    DriverError::Manifest {
        path: path.to_path_buf(),
        message: message.into(),
    }
}
