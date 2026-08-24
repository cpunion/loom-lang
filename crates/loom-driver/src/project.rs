//! Manifest, package-dependency, target, and source-root resolution.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub use loom_core::PackageId;

use crate::DriverError;
use crate::source::{
    discover_loom_files, has_loom_extension, is_ignored_relative, normalize_absolute, relative_key,
};

pub const MANIFEST_FILE: &str = "loom.toml";
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;
pub const LOCK_FILE: &str = "loom.lock";
pub const LOCK_SCHEMA_VERSION: u32 = 1;

/// How manifest resolution consults an existing lockfile.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LockMode {
    /// Reuse valid pins and allow the caller to materialize an updated lockfile.
    #[default]
    Use,
    /// Require the resolved graph to match the existing lockfile exactly.
    Locked,
    /// Ignore existing pins and resolve the newest matching registry versions.
    Refresh,
}

/// Explicit package-graph inputs which are independent of source semantics.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProjectOptions {
    pub features: BTreeSet<String>,
    pub no_default_features: bool,
    pub lock_mode: LockMode,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageDependency {
    alias: String,
    requirement: Option<String>,
    package: PackageId,
    source: String,
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

    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
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
    enabled_features: BTreeSet<String>,
    source: String,
    checksum: Option<String>,
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

    pub fn enabled_features(&self) -> impl Iterator<Item = &str> {
        self.enabled_features.iter().map(String::as_str)
    }

    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    #[must_use]
    pub fn checksum(&self) -> Option<&str> {
        self.checksum.as_deref()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetKind {
    Bin,
    Test,
    Lib,
}

impl TargetKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bin => "bin",
            Self::Test => "test",
            Self::Lib => "lib",
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

#[derive(Clone, Debug)]
struct LockState {
    path: PathBuf,
    rendered: String,
    current: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Lockfile {
    schema: u32,
    #[serde(default, rename = "package")]
    packages: Vec<LockedPackage>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct LockedPackage {
    name: String,
    version: String,
    source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checksum: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    dependencies: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    features: Vec<String>,
}

/// Fully resolved package dependency and target graph.
#[derive(Clone, Debug)]
pub struct ProjectGraph {
    root: PathBuf,
    manifest: Option<PathBuf>,
    kind: ProjectKind,
    packages: BTreeMap<PackageId, Package>,
    targets: Vec<Target>,
    lock: Option<LockState>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProjectSource {
    pub absolute: PathBuf,
    pub stable_path: String,
    pub package: Option<PackageId>,
    pub is_root_package: bool,
}

impl ProjectGraph {
    /// Resolves a manifest project or a legacy directory/single-file input.
    ///
    /// # Errors
    ///
    /// Returns a project error for unreadable inputs, invalid manifests,
    /// dependency cycles, or source roots which leave their package.
    pub fn load(input: impl AsRef<Path>) -> Result<Self, DriverError> {
        Self::load_with_options(input, &ProjectOptions::default())
    }

    /// Resolves a project with explicit feature and lockfile inputs.
    ///
    /// # Errors
    ///
    /// Returns a project error for an invalid feature graph, registry package,
    /// lockfile, manifest, dependency cycle, or source root.
    pub fn load_with_options(
        input: impl AsRef<Path>,
        options: &ProjectOptions,
    ) -> Result<Self, DriverError> {
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
            return Self::load_manifest(&manifest, options);
        }
        if metadata.is_dir() || metadata.is_file() && has_loom_extension(&canonical) {
            if !options.features.is_empty()
                || options.no_default_features
                || options.lock_mode != LockMode::Use
            {
                return Err(manifest_error(
                    &canonical,
                    "features and lockfile modes require a loom.toml project",
                ));
            }
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
                lock: None,
            });
        }
        Err(DriverError::InvalidRoot(canonical))
    }

    fn load_manifest(manifest: &Path, options: &ProjectOptions) -> Result<Self, DriverError> {
        let root = manifest
            .parent()
            .expect("manifest path has a parent")
            .to_path_buf();
        let lock_path = root.join(LOCK_FILE);
        let previous_lock = if options.lock_mode == LockMode::Refresh {
            None
        } else {
            read_lockfile(&lock_path)?
        };
        let mut resolver = Resolver::new(previous_lock.clone(), options.lock_mode);
        let request = FeatureRequest {
            names: options.features.clone(),
            use_default: !options.no_default_features,
        };
        let root_package =
            resolver.resolve(manifest, &mut Vec::new(), &request, &PackageOrigin::Root)?;
        let package = resolver
            .packages
            .get(&root_package)
            .expect("resolved root package is present");
        let generated_lock = lockfile_for_packages(&resolver.packages);
        let current = previous_lock
            .as_ref()
            .is_some_and(|previous| previous == &generated_lock);
        if options.lock_mode == LockMode::Locked && !current {
            return Err(manifest_error(
                &lock_path,
                format!(
                    "{LOCK_FILE} is missing or out of date; run `loomc resolve` before using --locked"
                ),
            ));
        }
        let rendered = toml::to_string(&generated_lock).map_err(|error| {
            manifest_error(&lock_path, format!("cannot encode lockfile: {error}"))
        })?;
        Ok(Self {
            root: package.root.clone(),
            manifest: Some(package.manifest.clone()),
            kind: ProjectKind::Manifest {
                root_package: root_package.clone(),
            },
            targets: package.targets.clone(),
            packages: resolver.packages,
            lock: Some(LockState {
                path: lock_path,
                rendered,
                current,
            }),
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
        self.root.join(format!(
            "target/loom/cache/v{}",
            crate::cache::CACHE_SCHEMA_VERSION
        ))
    }

    #[must_use]
    pub fn lockfile_path(&self) -> Option<&Path> {
        self.lock.as_ref().map(|lock| lock.path.as_path())
    }

    #[must_use]
    pub fn lockfile_is_current(&self) -> bool {
        self.lock.as_ref().is_none_or(|lock| lock.current)
    }

    /// Atomically materializes the deterministic resolved package graph.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the lockfile cannot be staged or published.
    pub fn write_lockfile(&self) -> Result<bool, DriverError> {
        let Some(lock) = &self.lock else {
            return Ok(false);
        };
        if lock.current
            || fs::read_to_string(&lock.path).is_ok_and(|existing| existing == lock.rendered)
        {
            return Ok(false);
        }
        let parent = lock.path.parent().unwrap_or_else(|| Path::new("."));
        let mut temporary =
            tempfile::NamedTempFile::new_in(parent).map_err(|source| DriverError::Io {
                path: lock.path.clone(),
                source,
            })?;
        temporary
            .write_all(lock.rendered.as_bytes())
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|source| DriverError::Io {
                path: lock.path.clone(),
                source,
            })?;
        temporary
            .persist(&lock.path)
            .map_err(|error| DriverError::Io {
                path: lock.path.clone(),
                source: error.error,
            })?;
        Ok(true)
    }

    pub(crate) fn source_files(&self) -> Result<Vec<ProjectSource>, DriverError> {
        match &self.kind {
            ProjectKind::Legacy { input } => legacy_sources(&self.root, input),
            ProjectKind::Manifest { root_package } => {
                let mut sources = BTreeMap::<PathBuf, (String, PackageId, bool)>::new();
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
                            if let Some((previous, _, _)) = sources.insert(
                                path.clone(),
                                (stable_path.clone(), package.id.clone(), is_root),
                            ) && previous != stable_path
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
                    .map(
                        |(absolute, (stable_path, package, is_root_package))| ProjectSource {
                            absolute,
                            stable_path,
                            package: Some(package),
                            is_root_package,
                        },
                    )
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
                    fields.push(format!(
                        "package:{}:{}:{}",
                        package.id,
                        package.source,
                        package.checksum.as_deref().unwrap_or("")
                    ));
                    for feature in &package.enabled_features {
                        fields.push(format!("feature:{}:{feature}", package.id));
                    }
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

    pub(crate) fn configure_hir_packages(&self, program: &mut loom_hir::Program) {
        match &self.kind {
            ProjectKind::Legacy { .. } => {
                program.register_package(PackageId::legacy(), [], true);
            }
            ProjectKind::Manifest { root_package } => {
                for package in self.packages.values() {
                    program.register_package(
                        package.id.clone(),
                        package.dependencies.iter().map(|dependency| {
                            (
                                loom_core::Name::new(dependency.alias.clone()),
                                dependency.package.clone(),
                            )
                        }),
                        &package.id == root_package,
                    );
                }
            }
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    schema: u32,
    package: RawPackage,
    #[serde(default)]
    dependencies: BTreeMap<String, RawDependency>,
    #[serde(default)]
    registries: BTreeMap<String, String>,
    #[serde(default)]
    features: BTreeMap<String, Vec<String>>,
    #[serde(default, rename = "target")]
    targets: Vec<RawTarget>,
}

#[derive(Clone, Deserialize)]
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

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDependency {
    path: Option<String>,
    registry: Option<String>,
    package: Option<String>,
    version: Option<String>,
    #[serde(default)]
    optional: bool,
    #[serde(default)]
    features: Vec<String>,
    #[serde(default = "default_true", rename = "default-features")]
    default_features: bool,
}

const fn default_true() -> bool {
    true
}

#[derive(Clone, Deserialize)]
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
    Lib,
}

#[derive(Clone, Debug)]
struct FeatureRequest {
    names: BTreeSet<String>,
    use_default: bool,
}

#[derive(Clone, Debug)]
enum PackageOrigin {
    Root,
    Path,
    Registry { name: String },
}

impl PackageOrigin {
    fn source(&self) -> String {
        match self {
            Self::Root => "root".to_owned(),
            Self::Path => "path".to_owned(),
            Self::Registry { name } => format!("registry+{name}"),
        }
    }
}

struct Resolver {
    packages: BTreeMap<PackageId, Package>,
    by_manifest: BTreeMap<PathBuf, PackageId>,
    enabled_features: BTreeMap<PathBuf, BTreeSet<String>>,
    locked: Option<Lockfile>,
    lock_mode: LockMode,
}

impl Resolver {
    fn new(locked: Option<Lockfile>, lock_mode: LockMode) -> Self {
        Self {
            packages: BTreeMap::new(),
            by_manifest: BTreeMap::new(),
            enabled_features: BTreeMap::new(),
            locked,
            lock_mode,
        }
    }
}

impl Resolver {
    fn resolve(
        &mut self,
        manifest: &Path,
        stack: &mut Vec<PathBuf>,
        request: &FeatureRequest,
        origin: &PackageOrigin,
    ) -> Result<PackageId, DriverError> {
        let manifest = canonical_manifest(manifest)?;
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

        let raw = read_manifest(&manifest)?;
        let id = PackageId::new(raw.package.name.clone(), raw.package.version.clone());
        let requested_features = resolve_features(&manifest, &raw, request)?;
        let mut combined_features = self
            .enabled_features
            .get(&manifest)
            .cloned()
            .unwrap_or_default();
        combined_features.extend(requested_features);
        if self.by_manifest.contains_key(&manifest)
            && self
                .enabled_features
                .get(&manifest)
                .is_some_and(|previous| previous == &combined_features)
        {
            return Ok(id);
        }
        let root = manifest
            .parent()
            .expect("canonical manifest has a parent")
            .to_path_buf();
        let source_roots = resolve_source_roots(&root, &manifest, raw.package.sources.clone())?;
        let targets = resolve_targets(&manifest, raw.targets.clone())?;

        stack.push(manifest.clone());
        let dependencies =
            self.resolve_dependencies(&manifest, &root, &raw, &combined_features, stack)?;
        stack.pop();

        let source = origin.source();
        let checksum = if matches!(origin, PackageOrigin::Registry { .. }) {
            Some(package_checksum(&manifest, &root, &source_roots)?)
        } else {
            None
        };
        self.verify_locked_checksum(&manifest, &id, &source, checksum.as_deref())?;

        if self.by_manifest.contains_key(&manifest) {
            let package = self
                .packages
                .get_mut(&id)
                .expect("resolved manifest has a package");
            package.dependencies = dependencies;
            package.enabled_features.clone_from(&combined_features);
            self.enabled_features.insert(manifest, combined_features);
            return Ok(id);
        }

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
                manifest: manifest.clone(),
                source_roots,
                dependencies,
                targets,
                enabled_features: combined_features.clone(),
                source,
                checksum,
            },
        );
        self.enabled_features.insert(manifest, combined_features);
        Ok(id)
    }

    fn resolve_dependencies(
        &mut self,
        manifest: &Path,
        root: &Path,
        raw: &RawManifest,
        enabled_features: &BTreeSet<String>,
        stack: &mut Vec<PathBuf>,
    ) -> Result<Vec<PackageDependency>, DriverError> {
        let mut dependencies = Vec::with_capacity(raw.dependencies.len());
        for (alias, dependency) in &raw.dependencies {
            validate_name("dependency alias", alias, manifest)?;
            if dependency.optional && !enabled_features.contains(&format!("dep:{alias}")) {
                continue;
            }
            dependencies
                .push(self.resolve_dependency(manifest, root, raw, alias, dependency, stack)?);
        }
        Ok(dependencies)
    }

    fn resolve_dependency(
        &mut self,
        manifest: &Path,
        root: &Path,
        raw: &RawManifest,
        alias: &str,
        dependency: &RawDependency,
        stack: &mut Vec<PathBuf>,
    ) -> Result<PackageDependency, DriverError> {
        let package_name = dependency.package.as_deref().unwrap_or(alias);
        validate_name("dependency package", package_name, manifest)?;
        let requirement = dependency
            .version
            .as_deref()
            .map(|value| parse_requirement(manifest, alias, value))
            .transpose()?;
        let (child_manifest, origin, registry_version) = self.dependency_location(
            manifest,
            root,
            raw,
            alias,
            dependency,
            package_name,
            requirement.as_ref(),
        )?;
        let request = FeatureRequest {
            names: dependency.features.iter().cloned().collect(),
            use_default: dependency.default_features,
        };
        let source = origin.source();
        let child = self.resolve(&child_manifest, stack, &request, &origin)?;
        if child.name() != package_name {
            return Err(manifest_error(
                manifest,
                format!(
                    "dependency `{alias}` requested package `{package_name}`, but `{child}` was resolved"
                ),
            ));
        }
        if registry_version
            .as_ref()
            .is_some_and(|version| version.to_string() != child.version())
        {
            return Err(manifest_error(
                &child_manifest,
                format!(
                    "registry directory version `{}` does not match package `{child}`",
                    registry_version.expect("registry version exists")
                ),
            ));
        }
        if let Some(requirement) = &requirement {
            let actual = Version::parse(child.version()).expect("package version was validated");
            if !requirement.matches(&actual) {
                return Err(manifest_error(
                    manifest,
                    format!(
                        "dependency `{alias}` requires `{}`, but `{child}` was resolved",
                        dependency.version.as_deref().expect("requirement exists")
                    ),
                ));
            }
        }
        Ok(PackageDependency {
            alias: alias.to_owned(),
            requirement: dependency.version.clone(),
            package: child,
            source,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn dependency_location(
        &self,
        manifest: &Path,
        root: &Path,
        raw: &RawManifest,
        alias: &str,
        dependency: &RawDependency,
        package_name: &str,
        requirement: Option<&VersionReq>,
    ) -> Result<(PathBuf, PackageOrigin, Option<Version>), DriverError> {
        match (&dependency.path, &dependency.registry) {
            (Some(path), None) => {
                let relative = manifest_relative_path(
                    manifest,
                    &format!("dependency `{alias}` path"),
                    path,
                    true,
                )?;
                Ok((
                    dependency_manifest(&root.join(relative))?,
                    PackageOrigin::Path,
                    None,
                ))
            }
            (None, Some(registry)) => {
                validate_name("registry", registry, manifest)?;
                let configured = raw.registries.get(registry).ok_or_else(|| {
                    manifest_error(
                        manifest,
                        format!("dependency `{alias}` uses unknown registry `{registry}`"),
                    )
                })?;
                let requirement = requirement.ok_or_else(|| {
                    manifest_error(
                        manifest,
                        format!("registry dependency `{alias}` requires a version"),
                    )
                })?;
                let relative = manifest_relative_path(
                    manifest,
                    &format!("registry `{registry}` path"),
                    configured,
                    true,
                )?;
                let registry_root =
                    fs::canonicalize(root.join(relative)).map_err(|source| DriverError::Io {
                        path: root.join(configured),
                        source,
                    })?;
                let locked = self.locked_registry_version(registry, package_name, requirement);
                let (manifest, version) = registry_dependency_manifest(
                    manifest,
                    &registry_root,
                    package_name,
                    requirement,
                    locked.as_ref(),
                )?;
                Ok((
                    manifest,
                    PackageOrigin::Registry {
                        name: registry.clone(),
                    },
                    Some(version),
                ))
            }
            (Some(_), Some(_)) => Err(manifest_error(
                manifest,
                format!("dependency `{alias}` cannot set both path and registry"),
            )),
            (None, None) => Err(manifest_error(
                manifest,
                format!("dependency `{alias}` must set exactly one of path or registry"),
            )),
        }
    }

    fn locked_registry_version(
        &self,
        registry: &str,
        package: &str,
        requirement: &VersionReq,
    ) -> Option<Version> {
        if self.lock_mode == LockMode::Refresh {
            return None;
        }
        let source = format!("registry+{registry}");
        self.locked.as_ref()?.packages.iter().find_map(|locked| {
            (locked.name == package && locked.source == source)
                .then(|| Version::parse(&locked.version).ok())
                .flatten()
                .filter(|version| requirement.matches(version))
        })
    }

    fn verify_locked_checksum(
        &self,
        manifest: &Path,
        package: &PackageId,
        source: &str,
        checksum: Option<&str>,
    ) -> Result<(), DriverError> {
        if self.lock_mode == LockMode::Refresh || !source.starts_with("registry+") {
            return Ok(());
        }
        let Some(locked) = self.locked.as_ref().and_then(|lock| {
            lock.packages.iter().find(|locked| {
                locked.name == package.name()
                    && locked.version == package.version()
                    && locked.source == source
            })
        }) else {
            return Ok(());
        };
        if locked.checksum.as_deref() != checksum {
            return Err(manifest_error(
                manifest,
                format!(
                    "registry package `{package}` checksum differs from {LOCK_FILE}; refuse mutable published versions"
                ),
            ));
        }
        Ok(())
    }
}

fn read_manifest(manifest: &Path) -> Result<RawManifest, DriverError> {
    let text = fs::read_to_string(manifest).map_err(|source| DriverError::Io {
        path: manifest.to_path_buf(),
        source,
    })?;
    let raw: RawManifest = toml::from_str(&text)
        .map_err(|error| manifest_error(manifest, format!("invalid TOML: {error}")))?;
    if raw.schema != MANIFEST_SCHEMA_VERSION {
        return Err(manifest_error(
            manifest,
            format!(
                "manifest schema {} is incompatible with supported schema {MANIFEST_SCHEMA_VERSION}",
                raw.schema
            ),
        ));
    }
    validate_name("package", &raw.package.name, manifest)?;
    Version::parse(&raw.package.version).map_err(|error| {
        manifest_error(
            manifest,
            format!(
                "package version `{}` is not SemVer: {error}",
                raw.package.version
            ),
        )
    })?;
    Ok(raw)
}

fn parse_requirement(
    manifest: &Path,
    alias: &str,
    requirement: &str,
) -> Result<VersionReq, DriverError> {
    VersionReq::parse(requirement).map_err(|error| {
        manifest_error(
            manifest,
            format!("dependency `{alias}` version requirement `{requirement}` is invalid: {error}"),
        )
    })
}

fn resolve_features(
    manifest: &Path,
    raw: &RawManifest,
    request: &FeatureRequest,
) -> Result<BTreeSet<String>, DriverError> {
    let mut states = BTreeMap::<String, u8>::new();
    let mut trail = Vec::new();
    for feature in raw.features.keys() {
        validate_name("feature", feature, manifest)?;
        validate_feature(manifest, raw, feature, &mut states, &mut trail)?;
    }
    let mut pending = request.names.iter().cloned().collect::<Vec<_>>();
    if request.use_default && raw.features.contains_key("default") {
        pending.push("default".to_owned());
    }
    let mut enabled = BTreeSet::new();
    while let Some(feature) = pending.pop() {
        if feature.starts_with("dep:") {
            return Err(manifest_error(
                manifest,
                format!("`{feature}` is an internal dependency activation, not a feature name"),
            ));
        }
        validate_name("feature", &feature, manifest)?;
        if !enabled.insert(feature.clone()) {
            continue;
        }
        let Some(members) = raw.features.get(&feature) else {
            return Err(manifest_error(
                manifest,
                format!("unknown feature `{feature}`"),
            ));
        };
        for member in members {
            if member.starts_with("dep:") {
                enabled.insert(member.clone());
            } else {
                pending.push(member.clone());
            }
        }
    }
    Ok(enabled)
}

fn validate_feature(
    manifest: &Path,
    raw: &RawManifest,
    feature: &str,
    states: &mut BTreeMap<String, u8>,
    trail: &mut Vec<String>,
) -> Result<(), DriverError> {
    match states.get(feature).copied() {
        Some(2) => return Ok(()),
        Some(1) => {
            let start = trail.iter().position(|name| name == feature).unwrap_or(0);
            let mut cycle = trail[start..].to_vec();
            cycle.push(feature.to_owned());
            return Err(manifest_error(
                manifest,
                format!("feature cycle: {}", cycle.join(" -> ")),
            ));
        }
        _ => {}
    }
    states.insert(feature.to_owned(), 1);
    trail.push(feature.to_owned());
    let members = raw
        .features
        .get(feature)
        .expect("feature validation starts from a declared feature");
    let mut seen = BTreeSet::new();
    for member in members {
        if !seen.insert(member) {
            return Err(manifest_error(
                manifest,
                format!("feature `{feature}` lists `{member}` more than once"),
            ));
        }
        if let Some(alias) = member.strip_prefix("dep:") {
            let Some(dependency) = raw.dependencies.get(alias) else {
                return Err(manifest_error(
                    manifest,
                    format!("feature `{feature}` references unknown dependency `{alias}`"),
                ));
            };
            if !dependency.optional {
                return Err(manifest_error(
                    manifest,
                    format!("feature `{feature}` references non-optional dependency `{alias}`"),
                ));
            }
        } else {
            validate_name("feature member", member, manifest)?;
            if !raw.features.contains_key(member) {
                return Err(manifest_error(
                    manifest,
                    format!("feature `{feature}` references unknown feature `{member}`"),
                ));
            }
            validate_feature(manifest, raw, member, states, trail)?;
        }
    }
    trail.pop();
    states.insert(feature.to_owned(), 2);
    Ok(())
}

fn registry_dependency_manifest(
    manifest: &Path,
    registry_root: &Path,
    package: &str,
    requirement: &VersionReq,
    locked: Option<&Version>,
) -> Result<(PathBuf, Version), DriverError> {
    let package_root = registry_root.join(package);
    let entries = fs::read_dir(&package_root).map_err(|source| DriverError::Io {
        path: package_root.clone(),
        source,
    })?;
    let mut candidates = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| DriverError::Io {
            path: package_root.clone(),
            source,
        })?;
        if !entry
            .file_type()
            .map_err(|source| DriverError::Io {
                path: entry.path(),
                source,
            })?
            .is_dir()
        {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            return Err(DriverError::NonUtf8Path(entry.path()));
        };
        let Ok(version) = Version::parse(&name) else {
            continue;
        };
        if requirement.matches(&version) && entry.path().join(MANIFEST_FILE).is_file() {
            candidates.push((version, entry.path().join(MANIFEST_FILE)));
        }
    }
    candidates.sort_by(|left, right| left.0.cmp(&right.0));
    let selected = if let Some(locked) = locked {
        candidates
            .iter()
            .find(|(version, _)| version == locked)
            .cloned()
    } else {
        candidates.pop()
    };
    selected.map(|(version, path)| (path, version)).ok_or_else(|| {
        let pin = locked.map_or_else(String::new, |version| format!(" locked to {version}"));
        manifest_error(
            manifest,
            format!(
                "registry package `{package}`{pin} has no version matching `{requirement}` in {}",
                registry_root.display()
            ),
        )
    })
}

fn package_checksum(
    manifest: &Path,
    package_root: &Path,
    source_roots: &[SourceRoot],
) -> Result<String, DriverError> {
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, b"loom-registry-package-v1");
    hash_field(
        &mut hasher,
        &fs::read(manifest).map_err(|source| DriverError::Io {
            path: manifest.to_path_buf(),
            source,
        })?,
    );
    let mut paths = BTreeSet::new();
    for root in source_roots {
        paths.extend(discover_package_source(root, package_root)?);
    }
    for path in paths {
        let relative = relative_key(package_root, &path)
            .ok_or_else(|| DriverError::NonUtf8Path(path.clone()))?;
        hash_field(&mut hasher, relative.as_bytes());
        hash_field(
            &mut hasher,
            &fs::read(&path).map_err(|source| DriverError::Io {
                path: path.clone(),
                source,
            })?,
        );
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn hash_field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn read_lockfile(path: &Path) -> Result<Option<Lockfile>, DriverError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(DriverError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let lock: Lockfile = toml::from_str(&text)
        .map_err(|error| manifest_error(path, format!("invalid lockfile: {error}")))?;
    if lock.schema != LOCK_SCHEMA_VERSION {
        return Err(manifest_error(
            path,
            format!(
                "lockfile schema {} is incompatible with supported schema {LOCK_SCHEMA_VERSION}",
                lock.schema
            ),
        ));
    }
    let mut identities = BTreeSet::new();
    for package in &lock.packages {
        validate_name("locked package", &package.name, path)?;
        Version::parse(&package.version).map_err(|error| {
            manifest_error(
                path,
                format!(
                    "locked package `{}@{}` has an invalid version: {error}",
                    package.name, package.version
                ),
            )
        })?;
        if !identities.insert((&package.name, &package.version, &package.source)) {
            return Err(manifest_error(
                path,
                format!(
                    "duplicate locked package `{}@{}` from `{}`",
                    package.name, package.version, package.source
                ),
            ));
        }
        if package.source.starts_with("registry+")
            && package.checksum.as_deref().is_none_or(|checksum| {
                checksum.len() != 64 || !checksum.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        {
            return Err(manifest_error(
                path,
                format!(
                    "registry package `{}@{}` requires a SHA-256 checksum",
                    package.name, package.version
                ),
            ));
        }
    }
    Ok(Some(lock))
}

fn lockfile_for_packages(packages: &BTreeMap<PackageId, Package>) -> Lockfile {
    let packages = packages
        .values()
        .map(|package| LockedPackage {
            name: package.id.name().to_owned(),
            version: package.id.version().to_owned(),
            source: package.source.clone(),
            checksum: package.checksum.clone(),
            dependencies: package
                .dependencies
                .iter()
                .map(|dependency| dependency.package.to_string())
                .collect(),
            features: package.enabled_features.iter().cloned().collect(),
        })
        .collect();
    Lockfile {
        schema: LOCK_SCHEMA_VERSION,
        packages,
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
            RawTargetKind::Lib => {
                if raw.entry.is_some() {
                    return Err(manifest_error(
                        manifest,
                        format!("library target `{}` cannot declare entry", raw.name),
                    ));
                }
                (TargetKind::Lib, None)
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
                package: None,
                is_root_package: true,
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
