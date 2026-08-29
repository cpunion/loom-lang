//! Authenticated HTTP package-registry transport and local materialization.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::net::IpAddr;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::project::MANIFEST_SCHEMA_VERSION;
use crate::{DriverError, MANIFEST_FILE, ProjectGraph, SourceMap};

const REGISTRY_PROTOCOL_VERSION: u32 = 1;
const MAX_HTTP_RESPONSE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_BUNDLE_FILES: usize = 4096;
const MAX_BUNDLE_FILE_BYTES: usize = 16 * 1024 * 1024;
const MIN_AUTH_TOKEN_BYTES: usize = 16;
const MAX_AUTH_TOKEN_BYTES: usize = 8 * 1024;
const CACHE_RECORD_FILE: &str = ".loom-registry.json";
const CACHED_BUNDLE_FILE: &str = ".loom-registry-bundle.json";

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum RegistryConfig {
    Path(String),
    Http(HttpRegistryConfig),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct HttpRegistryConfig {
    pub url: String,
    pub token_env: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RegistryPublish {
    pub registry: String,
    pub package: String,
    pub version: String,
    pub sha256: String,
    pub endpoint: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryIndex {
    schema_version: u32,
    versions: Vec<RegistryRelease>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryRelease {
    version: String,
    sha256: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RegistryBundle {
    schema_version: u32,
    package: String,
    version: String,
    language: String,
    files: Vec<RegistryFile>,
}

#[derive(Deserialize)]
struct BundleManifest {
    schema: u32,
    language: Option<String>,
    module: BundleManifestModule,
}

#[derive(Deserialize)]
struct BundleManifestModule {
    name: String,
    version: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RegistryFile {
    path: String,
    text: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RegistryCacheRecord {
    schema_version: u32,
    registry_url: String,
    package: String,
    version: String,
    bundle_sha256: String,
}

#[derive(Deserialize)]
struct PublishManifest {
    #[serde(default)]
    registries: BTreeMap<String, RegistryConfig>,
}

struct HttpResponse {
    status: u16,
    body: Vec<u8>,
}

/// Publishes the root package as one deterministic source bundle.
///
/// # Errors
///
/// Returns a project error for invalid configuration, missing auth, transport
/// failure, or a non-successful registry response.
pub fn publish_registry_package(
    input: impl AsRef<Path>,
    registry: &str,
) -> Result<RegistryPublish, DriverError> {
    let project = ProjectGraph::load(input)?;
    let manifest = project.manifest().ok_or_else(|| DriverError::Manifest {
        path: project.root().to_path_buf(),
        message: "registry publish requires a loom.toml project".to_owned(),
    })?;
    let raw_text = fs::read_to_string(manifest).map_err(|source| DriverError::Io {
        path: manifest.to_path_buf(),
        source,
    })?;
    let raw = toml::from_str::<PublishManifest>(&raw_text).map_err(|error| {
        registry_error(
            manifest,
            format!("cannot read registry configuration: {error}"),
        )
    })?;
    let config = raw.registries.get(registry).ok_or_else(|| {
        registry_error(
            manifest,
            format!("package does not configure registry `{registry}`"),
        )
    })?;
    let RegistryConfig::Http(config) = config else {
        return Err(registry_error(
            manifest,
            format!("registry `{registry}` is a filesystem registry and cannot accept publish"),
        ));
    };
    validate_registry_config(manifest, config)?;
    let package = project.root_package().ok_or_else(|| {
        registry_error(
            manifest,
            "registry publish requires a manifest root package",
        )
    })?;
    let sources = SourceMap::load(&project, &BTreeMap::new())?;
    let mut files = vec![RegistryFile {
        path: MANIFEST_FILE.to_owned(),
        text: raw_text,
    }];
    for source in sources
        .documents()
        .iter()
        .filter(|source| source.is_root_package())
    {
        let text = source.text().ok_or_else(|| DriverError::Manifest {
            path: source.absolute_path().to_path_buf(),
            message: "registry packages require UTF-8 source files".to_owned(),
        })?;
        files.push(RegistryFile {
            path: source.relative_path().to_owned(),
            text: text.to_owned(),
        });
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    let bundle = RegistryBundle {
        schema_version: REGISTRY_PROTOCOL_VERSION,
        package: package.id().name().to_owned(),
        version: package.id().version().to_owned(),
        language: package.language_version().to_owned(),
        files,
    };
    validate_bundle(
        manifest,
        &bundle,
        package.id().name(),
        package.id().version(),
    )?;
    let bytes = serde_json::to_vec(&bundle).map_err(|error| {
        registry_error(manifest, format!("cannot encode registry bundle: {error}"))
    })?;
    let sha256 = digest(&bytes);
    let endpoint = package_endpoint(&config.url, &bundle.package, &bundle.version);
    let token = registry_token(config).map_err(|message| registry_error(manifest, message))?;
    if token
        .as_deref()
        .is_some_and(|token| response_contains_secret(&bytes, token))
    {
        return Err(registry_error(
            manifest,
            "registry package was rejected because its source bundle contains credential data",
        ));
    }
    let response = http_request("PUT", &endpoint, token.as_deref(), Some((&bytes, &sha256)))
        .map_err(|message| registry_error(manifest, message))?;
    if !matches!(response.status, 200 | 201 | 204) {
        return Err(http_status_error(manifest, "publish", response.status));
    }
    Ok(RegistryPublish {
        registry: registry.to_owned(),
        package: bundle.package,
        version: bundle.version,
        sha256,
        endpoint,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn fetch_http_registry_package(
    manifest: &Path,
    root: &Path,
    registry: &str,
    config: &HttpRegistryConfig,
    package: &str,
    requirement: &VersionReq,
    locked: Option<&Version>,
    refresh: bool,
    offline: bool,
) -> Result<(PathBuf, Version), DriverError> {
    validate_registry_config(manifest, config)?;
    let cache_root = registry_cache_root(root, registry, &config.url, package);
    if !refresh
        && let Some(locked) = locked
        && let Some(cached) = find_cached_package(&cache_root, &config.url, package, locked)?
    {
        return Ok((cached, locked.clone()));
    }
    if offline {
        return Err(DriverError::OfflineRegistryMiss {
            path: manifest.to_path_buf(),
            package: package.to_owned(),
            version: locked.map(ToString::to_string),
        });
    }

    let token = registry_token(config).map_err(|message| registry_error(manifest, message))?;
    let index_endpoint = index_endpoint(&config.url, package);
    let response = http_request("GET", &index_endpoint, token.as_deref(), None)
        .map_err(|message| registry_error(manifest, message))?;
    if response.status != 200 {
        return Err(http_status_error(manifest, "fetch index", response.status));
    }
    let index = serde_json::from_slice::<RegistryIndex>(&response.body).map_err(|error| {
        registry_error(
            manifest,
            format!("registry `{registry}` returned an invalid index for `{package}`: {error}"),
        )
    })?;
    if index.schema_version != REGISTRY_PROTOCOL_VERSION {
        return Err(registry_error(
            manifest,
            format!(
                "registry `{registry}` index schema {} is incompatible with supported schema {REGISTRY_PROTOCOL_VERSION}",
                index.schema_version
            ),
        ));
    }
    let (version, bundle_sha256) = select_release(
        manifest,
        registry,
        package,
        requirement,
        locked,
        index.versions,
    )?;
    let version_root = cache_root.join(version.to_string());
    if let Some(cached) = cached_for_digest(
        &version_root,
        &config.url,
        package,
        &version,
        &bundle_sha256,
    )? {
        return Ok((cached, version));
    }
    if version_root.is_dir()
        && fs::read_dir(&version_root).is_ok_and(|mut entries| entries.next().is_some())
    {
        return Err(registry_error(
            manifest,
            format!(
                "registry `{registry}` changed immutable package `{package}@{version}`; clear its registry cache only after verifying the registry"
            ),
        ));
    }

    let endpoint = package_endpoint(&config.url, package, &version.to_string());
    let response = http_request("GET", &endpoint, token.as_deref(), None)
        .map_err(|message| registry_error(manifest, message))?;
    if response.status != 200 {
        return Err(http_status_error(
            manifest,
            "fetch package",
            response.status,
        ));
    }
    let actual_sha256 = digest(&response.body);
    if actual_sha256 != bundle_sha256 {
        return Err(registry_error(
            manifest,
            format!(
                "registry package `{package}@{version}` SHA-256 mismatch: index declared {bundle_sha256}, response was {actual_sha256}"
            ),
        ));
    }
    let bundle = serde_json::from_slice::<RegistryBundle>(&response.body).map_err(|error| {
        registry_error(
            manifest,
            format!("registry package `{package}@{version}` is invalid: {error}"),
        )
    })?;
    validate_bundle(manifest, &bundle, package, &version.to_string())?;
    let materialized = materialize_bundle(
        manifest,
        &version_root,
        &config.url,
        &bundle_sha256,
        &response.body,
        &bundle,
    )?;
    Ok((materialized, version))
}

fn select_release(
    manifest: &Path,
    registry: &str,
    package: &str,
    requirement: &VersionReq,
    locked: Option<&Version>,
    releases: Vec<RegistryRelease>,
) -> Result<(Version, String), DriverError> {
    let mut candidates = Vec::new();
    let mut seen_versions = BTreeSet::new();
    for release in releases {
        let version = Version::parse(&release.version).map_err(|error| {
            registry_error(
                manifest,
                format!(
                    "registry `{registry}` index has an invalid version `{}` for `{package}`: {error}",
                    release.version
                ),
            )
        })?;
        if !seen_versions.insert(version.clone()) {
            return Err(registry_error(
                manifest,
                format!("registry `{registry}` index repeats `{package}@{version}`"),
            ));
        }
        if !valid_digest(&release.sha256) {
            return Err(registry_error(
                manifest,
                format!(
                    "registry `{registry}` index has an invalid SHA-256 for `{package}@{version}`"
                ),
            ));
        }
        if requirement.matches(&version) && locked.is_none_or(|locked| locked == &version) {
            candidates.push((version, release.sha256.to_ascii_lowercase()));
        }
    }
    candidates.sort_by(|left, right| left.0.cmp(&right.0));
    candidates.pop().ok_or_else(|| {
        let pin = locked.map_or_else(String::new, |version| format!(" locked to {version}"));
        registry_error(
            manifest,
            format!(
                "registry `{registry}` package `{package}`{pin} has no version matching `{requirement}`"
            ),
        )
    })
}

#[allow(clippy::too_many_lines)]
fn validate_bundle(
    manifest: &Path,
    bundle: &RegistryBundle,
    package: &str,
    version: &str,
) -> Result<(), DriverError> {
    if bundle.schema_version != REGISTRY_PROTOCOL_VERSION {
        return Err(registry_error(
            manifest,
            format!(
                "registry bundle schema {} is incompatible with supported schema {REGISTRY_PROTOCOL_VERSION}",
                bundle.schema_version
            ),
        ));
    }
    if bundle.package != package || bundle.version != version {
        return Err(registry_error(
            manifest,
            format!(
                "registry bundle identity `{}@{}` does not match requested `{package}@{version}`",
                bundle.package, bundle.version
            ),
        ));
    }
    if bundle.language != crate::CURRENT_LANGUAGE_VERSION {
        return Err(DriverError::UnsupportedLanguageVersion {
            path: manifest.to_path_buf(),
            found: bundle.language.clone(),
            supported: crate::CURRENT_LANGUAGE_VERSION,
        });
    }
    if bundle.files.is_empty() || bundle.files.len() > MAX_BUNDLE_FILES {
        return Err(registry_error(
            manifest,
            format!("registry bundle must contain 1..={MAX_BUNDLE_FILES} files"),
        ));
    }
    let mut paths = BTreeSet::new();
    let mut normalized_paths = BTreeSet::new();
    for file in &bundle.files {
        let Some(normalized) = safe_bundle_path(&file.path) else {
            return Err(registry_error(
                manifest,
                format!(
                    "registry bundle path `{}` is not a safe relative path",
                    file.path
                ),
            ));
        };
        if matches!(file.path.as_str(), CACHE_RECORD_FILE | CACHED_BUNDLE_FILE) {
            return Err(registry_error(
                manifest,
                format!("registry bundle path `{}` is reserved", file.path),
            ));
        }
        if !paths.insert(file.path.as_str()) {
            return Err(registry_error(
                manifest,
                format!("registry bundle repeats path `{}`", file.path),
            ));
        }
        if normalized_paths
            .iter()
            .any(|path: &PathBuf| normalized.starts_with(path) || path.starts_with(&normalized))
        {
            return Err(registry_error(
                manifest,
                format!(
                    "registry bundle path `{}` conflicts with another file path",
                    file.path
                ),
            ));
        }
        normalized_paths.insert(normalized);
        if file.text.len() > MAX_BUNDLE_FILE_BYTES {
            return Err(registry_error(
                manifest,
                format!(
                    "registry bundle file `{}` exceeds {MAX_BUNDLE_FILE_BYTES} bytes",
                    file.path
                ),
            ));
        }
    }
    if !paths.contains(MANIFEST_FILE) {
        return Err(registry_error(
            manifest,
            format!("registry bundle does not contain {MANIFEST_FILE}"),
        ));
    }
    let manifest_file = bundle
        .files
        .iter()
        .find(|file| file.path == MANIFEST_FILE)
        .expect("bundle manifest presence was checked");
    let embedded = toml::from_str::<BundleManifest>(&manifest_file.text).map_err(|error| {
        registry_error(
            manifest,
            format!("registry bundle contains an invalid {MANIFEST_FILE}: {error}"),
        )
    })?;
    let embedded_language = embedded
        .language
        .as_deref()
        .unwrap_or(crate::CURRENT_LANGUAGE_VERSION);
    if embedded.schema != MANIFEST_SCHEMA_VERSION
        || embedded.module.name != bundle.package
        || embedded.module.version != bundle.version
        || embedded_language != bundle.language
    {
        return Err(registry_error(
            manifest,
            format!("registry bundle metadata does not match the identity in {MANIFEST_FILE}"),
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn materialize_bundle(
    manifest: &Path,
    version_root: &Path,
    registry_url: &str,
    bundle_sha256: &str,
    bundle_bytes: &[u8],
    bundle: &RegistryBundle,
) -> Result<PathBuf, DriverError> {
    if digest(bundle_bytes) != bundle_sha256 {
        return Err(registry_error(
            manifest,
            "registry bundle bytes do not match their declared SHA-256",
        ));
    }
    let package_root = version_root
        .parent()
        .expect("versioned registry cache has a package parent");
    fs::create_dir_all(package_root).map_err(|source| DriverError::Io {
        path: package_root.to_path_buf(),
        source,
    })?;
    let final_root = version_root.join(bundle_sha256);
    if final_root.exists() {
        let version = Version::parse(&bundle.version).map_err(|error| {
            registry_error(
                manifest,
                format!("registry bundle has an invalid version: {error}"),
            )
        })?;
        return valid_cached_record(
            &final_root,
            registry_url,
            &bundle.package,
            &version,
            Some(bundle_sha256),
        )?
        .ok_or_else(|| invalid_cache(&final_root, "existing cache materialization is incomplete"));
    }
    let staging = tempfile::Builder::new()
        .prefix(".loom-registry-")
        .tempdir_in(package_root)
        .map_err(|source| DriverError::Io {
            path: package_root.to_path_buf(),
            source,
        })?;
    for file in &bundle.files {
        let relative = safe_bundle_path(&file.path).expect("validated bundle path");
        let path = staging.path().join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| DriverError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        fs::write(&path, file.text.as_bytes())
            .map_err(|source| DriverError::Io { path, source })?;
    }
    let record = RegistryCacheRecord {
        schema_version: REGISTRY_PROTOCOL_VERSION,
        registry_url: registry_url.trim_end_matches('/').to_owned(),
        package: bundle.package.clone(),
        version: bundle.version.clone(),
        bundle_sha256: bundle_sha256.to_owned(),
    };
    let record_bytes = serde_json::to_vec(&record).map_err(|error| {
        registry_error(
            manifest,
            format!("cannot encode registry cache record: {error}"),
        )
    })?;
    fs::write(staging.path().join(CACHE_RECORD_FILE), record_bytes).map_err(|source| {
        DriverError::Io {
            path: staging.path().join(CACHE_RECORD_FILE),
            source,
        }
    })?;
    fs::write(staging.path().join(CACHED_BUNDLE_FILE), bundle_bytes).map_err(|source| {
        DriverError::Io {
            path: staging.path().join(CACHED_BUNDLE_FILE),
            source,
        }
    })?;
    if !materialized_bundle_matches(staging.path(), bundle)? {
        return Err(registry_error(
            manifest,
            "staged registry package does not match its authenticated bundle",
        ));
    }
    fs::create_dir_all(version_root).map_err(|source| DriverError::Io {
        path: version_root.to_path_buf(),
        source,
    })?;
    match fs::rename(staging.path(), &final_root) {
        Ok(()) => {}
        Err(_error) if final_root.exists() => {
            let version = Version::parse(&bundle.version).map_err(|error| {
                registry_error(
                    manifest,
                    format!("registry bundle has an invalid version: {error}"),
                )
            })?;
            let Some(existing) = valid_cached_record(
                &final_root,
                registry_url,
                &bundle.package,
                &version,
                Some(bundle_sha256),
            )?
            else {
                return Err(invalid_cache(
                    &final_root,
                    "concurrent cache materialization did not produce a complete package",
                ));
            };
            return Ok(existing);
        }
        Err(source) => {
            return Err(DriverError::Io {
                path: final_root,
                source,
            });
        }
    }
    Ok(final_root.join(MANIFEST_FILE))
}

fn find_cached_package(
    cache_root: &Path,
    registry_url: &str,
    package: &str,
    version: &Version,
) -> Result<Option<PathBuf>, DriverError> {
    let version_root = cache_root.join(version.to_string());
    let entries = match fs::read_dir(&version_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(DriverError::Io {
                path: version_root,
                source,
            });
        }
    };
    let mut matches = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| DriverError::Io {
            path: version_root.clone(),
            source,
        })?;
        let entry_path = entry.path();
        let Some(entry_digest) = entry.file_name().to_str().map(str::to_owned) else {
            return Err(invalid_cache(&entry_path, "cache entry name is not UTF-8"));
        };
        if !valid_digest(&entry_digest) {
            return Err(invalid_cache(
                &entry_path,
                "cache entry name is not a SHA-256 digest",
            ));
        }
        if let Some(manifest) = valid_cached_record(
            &entry_path,
            registry_url,
            package,
            version,
            Some(&entry_digest),
        )? {
            matches.push(manifest);
        }
    }
    matches.sort();
    if matches.len() > 1 {
        return Err(registry_error(
            &version_root,
            format!(
                "registry cache contains multiple immutable releases for `{package}@{version}`"
            ),
        ));
    }
    Ok(matches.pop())
}

fn cached_for_digest(
    version_root: &Path,
    registry_url: &str,
    package: &str,
    version: &Version,
    bundle_sha256: &str,
) -> Result<Option<PathBuf>, DriverError> {
    valid_cached_record(
        &version_root.join(bundle_sha256),
        registry_url,
        package,
        version,
        Some(bundle_sha256),
    )
}

fn valid_cached_record(
    root: &Path,
    registry_url: &str,
    package: &str,
    version: &Version,
    bundle_sha256: Option<&str>,
) -> Result<Option<PathBuf>, DriverError> {
    if !root.exists() {
        return Ok(None);
    }
    if !root.is_dir() {
        return Err(invalid_cache(root, "cache entry is not a directory"));
    }
    let manifest = root.join(MANIFEST_FILE);
    let bytes = read_validated_cache_file(root, CACHE_RECORD_FILE, 64 * 1024)?;
    let record = serde_json::from_slice::<RegistryCacheRecord>(&bytes)
        .map_err(|error| invalid_cache(root, format!("invalid cache record: {error}")))?;
    let valid = record.schema_version == REGISTRY_PROTOCOL_VERSION
        && record.registry_url == registry_url.trim_end_matches('/')
        && record.package == package
        && record.version == version.to_string()
        && bundle_sha256.is_none_or(|digest| record.bundle_sha256 == digest)
        && valid_digest(&record.bundle_sha256)
        && manifest.is_file();
    if !valid {
        return Err(invalid_cache(
            root,
            "cache identity does not match registry request",
        ));
    }
    let bundle_bytes =
        read_validated_cache_file(root, CACHED_BUNDLE_FILE, MAX_HTTP_RESPONSE_BYTES)?;
    if digest(&bundle_bytes) != record.bundle_sha256 {
        return Err(invalid_cache(root, "cached bundle SHA-256 mismatch"));
    }
    let bundle = serde_json::from_slice::<RegistryBundle>(&bundle_bytes)
        .map_err(|error| invalid_cache(root, format!("invalid cached bundle: {error}")))?;
    validate_bundle(root, &bundle, package, &version.to_string())
        .map_err(|error| invalid_cache(root, error.to_string()))?;
    if !materialized_bundle_matches(root, &bundle)? {
        return Err(invalid_cache(
            root,
            "materialized files do not match the authenticated bundle",
        ));
    }
    Ok(Some(manifest))
}

fn materialized_bundle_matches(root: &Path, bundle: &RegistryBundle) -> Result<bool, DriverError> {
    let expected = bundle
        .files
        .iter()
        .map(|file| (PathBuf::from(&file.path), file.text.as_bytes()))
        .collect::<BTreeMap<_, _>>();
    for (relative, bytes) in &expected {
        let path = root.join(relative);
        let metadata = fs::symlink_metadata(&path).map_err(|source| DriverError::Io {
            path: path.clone(),
            source,
        })?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() != u64::try_from(bytes.len()).unwrap_or(u64::MAX)
            || metadata.len() > u64::try_from(MAX_BUNDLE_FILE_BYTES).unwrap_or(u64::MAX)
        {
            return Ok(false);
        }
        let actual = fs::read(&path).map_err(|source| DriverError::Io {
            path: path.clone(),
            source,
        })?;
        if actual != *bytes {
            return Ok(false);
        }
    }
    let mut pending = vec![root.to_path_buf()];
    let mut seen = BTreeSet::new();
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory).map_err(|source| DriverError::Io {
            path: directory.clone(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| DriverError::Io {
                path: directory.clone(),
                source,
            })?;
            let metadata =
                fs::symlink_metadata(entry.path()).map_err(|source| DriverError::Io {
                    path: entry.path(),
                    source,
                })?;
            if metadata.file_type().is_symlink() {
                return Ok(false);
            }
            if metadata.is_dir() {
                pending.push(entry.path());
                continue;
            }
            if !metadata.is_file() {
                return Ok(false);
            }
            let relative = entry
                .path()
                .strip_prefix(root)
                .expect("walked cache path is below root")
                .to_path_buf();
            if relative == Path::new(CACHE_RECORD_FILE) || relative == Path::new(CACHED_BUNDLE_FILE)
            {
                continue;
            }
            if !expected.contains_key(&relative) {
                return Ok(false);
            }
            seen.insert(relative);
        }
    }
    Ok(seen.len() == expected.len())
}

fn read_validated_cache_file(
    root: &Path,
    name: &str,
    maximum: u64,
) -> Result<Vec<u8>, DriverError> {
    let path = root.join(name);
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| invalid_cache(root, format!("cannot inspect `{name}`: {error}")))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > maximum {
        return Err(invalid_cache(
            root,
            format!("`{name}` is not a bounded regular file"),
        ));
    }
    fs::read(&path).map_err(|error| invalid_cache(root, format!("cannot read `{name}`: {error}")))
}

fn invalid_cache(path: &Path, message: impl Into<String>) -> DriverError {
    registry_error(
        path,
        format!("registry cache validation failed: {}", message.into()),
    )
}

fn registry_cache_root(root: &Path, registry: &str, url: &str, package: &str) -> PathBuf {
    let identity = digest(format!("{registry}\0{}", url.trim_end_matches('/')).as_bytes());
    root.join("target/loom/registry/http")
        .join(identity)
        .join(package)
}

fn safe_bundle_path(path: &str) -> Option<PathBuf> {
    if path.contains('\\')
        || path.contains('\0')
        || path
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        return None;
    }
    let path = Path::new(path);
    (!path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_))))
    .then(|| path.to_path_buf())
}

fn validate_registry_config(
    manifest: &Path,
    config: &HttpRegistryConfig,
) -> Result<(), DriverError> {
    let secure = validate_registry_url(manifest, &config.url)?;
    if !secure && config.token_env.is_some() {
        return Err(registry_error(
            manifest,
            "registry authentication tokens require HTTPS; token-env cannot be used with loopback HTTP",
        ));
    }
    if config
        .token_env
        .as_deref()
        .is_some_and(|name| name.is_empty() || name.contains(['=', '\0']))
    {
        return Err(registry_error(
            manifest,
            "registry token-env must name a non-empty environment variable",
        ));
    }
    Ok(())
}

fn validate_registry_url(manifest: &Path, url: &str) -> Result<bool, DriverError> {
    let (secure, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (false, rest)
    } else {
        return Err(registry_error(
            manifest,
            "registry URL must use https:// (http:// is allowed only for loopback tests)",
        ));
    };
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty()
        || authority.contains('@')
        || rest.contains(['#', '?', '\\'])
        || !secure && !is_loopback_authority(authority)
    {
        return Err(registry_error(
            manifest,
            format!("registry URL `{url}` must be credential-free HTTPS or loopback-only HTTP"),
        ));
    }
    validate_authority(manifest, url, authority)?;
    Ok(secure)
}

fn is_loopback_authority(authority: &str) -> bool {
    authority_host(authority)
        .and_then(|host| host.parse::<IpAddr>().ok())
        .is_some_and(|address| address.is_loopback())
}

fn validate_authority(manifest: &Path, url: &str, authority: &str) -> Result<(), DriverError> {
    let Some(host) = authority_host(authority) else {
        return Err(registry_error(
            manifest,
            format!("registry URL `{url}` has an invalid authority"),
        ));
    };
    if host.is_empty()
        || host
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return Err(registry_error(
            manifest,
            format!("registry URL `{url}` has an invalid host"),
        ));
    }
    Ok(())
}

fn authority_host(authority: &str) -> Option<&str> {
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, suffix) = rest.split_once(']')?;
        valid_port_suffix(suffix).then_some(host)
    } else {
        if authority.matches(':').count() > 1 {
            return None;
        }
        if let Some((host, port)) = authority.split_once(':') {
            valid_port(port).then_some(host)
        } else {
            Some(authority)
        }
    }
}

fn valid_port_suffix(suffix: &str) -> bool {
    if suffix.is_empty() {
        return true;
    }
    let Some(port) = suffix.strip_prefix(':') else {
        return false;
    };
    valid_port(port)
}

fn valid_port(port: &str) -> bool {
    !port.is_empty()
        && port.bytes().all(|byte| byte.is_ascii_digit())
        && port.parse::<u16>().is_ok_and(|port| port != 0)
}

fn registry_token(config: &HttpRegistryConfig) -> Result<Option<String>, String> {
    let Some(variable) = config.token_env.as_deref() else {
        return Ok(None);
    };
    let token = std::env::var(variable).map_err(|_| {
        format!("registry authentication environment variable `{variable}` is not set")
    })?;
    validate_registry_token(variable, &token)?;
    Ok(Some(token))
}

fn validate_registry_token(variable: &str, token: &str) -> Result<(), String> {
    if token.len() < MIN_AUTH_TOKEN_BYTES
        || token.len() > MAX_AUTH_TOKEN_BYTES
        || !token.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'+' | b'/' | b'=')
        })
    {
        return Err(format!(
            "registry authentication environment variable `{variable}` is not a valid bearer token"
        ));
    }
    Ok(())
}

fn index_endpoint(base: &str, package: &str) -> String {
    format!(
        "{}/v1/packages/{}",
        base.trim_end_matches('/'),
        encode_component(package)
    )
}

fn package_endpoint(base: &str, package: &str, version: &str) -> String {
    format!(
        "{}/versions/{}",
        index_endpoint(base, package),
        encode_component(version)
    )
}

fn encode_component(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

fn http_request(
    method: &str,
    url: &str,
    token: Option<&str>,
    body: Option<(&[u8], &str)>,
) -> Result<HttpResponse, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .max_redirects(0)
        .build()
        .into();
    let media_type = "application/vnd.loom.registry+json; version=1";
    let response = match (method, body) {
        ("GET", None) => {
            let request = agent
                .get(url)
                .header("Accept", media_type)
                .header("Accept-Encoding", "identity");
            let request = if let Some(token) = token {
                request.header("Authorization", &format!("Bearer {token}"))
            } else {
                request
            };
            request.call()
        }
        ("PUT", Some((body, sha256))) => {
            let request = agent
                .put(url)
                .header("Accept", media_type)
                .header("Accept-Encoding", "identity")
                .header("Content-Type", media_type)
                .header("X-Loom-SHA256", sha256);
            let request = if let Some(token) = token {
                request.header("Authorization", &format!("Bearer {token}"))
            } else {
                request
            };
            request.send(body)
        }
        _ => return Err("unsupported registry HTTP request".to_owned()),
    }
    .map_err(|error| {
        if token.is_some() {
            format!("authenticated registry request to `{url}` failed")
        } else {
            format!("registry request to `{url}` failed: {error}")
        }
    })?;
    let status = response.status().as_u16();
    let (_, body) = response.into_parts();
    let mut bytes = Vec::new();
    body.into_reader()
        .take(MAX_HTTP_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read registry response: {error}"))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_HTTP_RESPONSE_BYTES {
        return Err(format!(
            "registry response exceeds {MAX_HTTP_RESPONSE_BYTES} bytes"
        ));
    }
    if token.is_some_and(|token| response_contains_secret(&bytes, token)) {
        return Err(format!(
            "authenticated registry response from `{url}` was rejected because it reflected credential data"
        ));
    }
    Ok(HttpResponse {
        status,
        body: bytes,
    })
}

fn response_contains_secret(bytes: &[u8], secret: &str) -> bool {
    if secret.is_empty() {
        return false;
    }
    if bytes
        .windows(secret.len())
        .any(|window| window == secret.as_bytes())
    {
        return true;
    }
    serde_json::from_slice::<serde_json::Value>(bytes)
        .is_ok_and(|value| json_contains_secret(&value, secret))
}

fn json_contains_secret(value: &serde_json::Value, secret: &str) -> bool {
    match value {
        serde_json::Value::String(value) => value.contains(secret),
        serde_json::Value::Array(values) => values
            .iter()
            .any(|value| json_contains_secret(value, secret)),
        serde_json::Value::Object(fields) => fields
            .iter()
            .any(|(name, value)| name.contains(secret) || json_contains_secret(value, secret)),
        _ => false,
    }
}

fn http_status_error(manifest: &Path, operation: &str, status: u16) -> DriverError {
    registry_error(
        manifest,
        format!("registry {operation} failed with HTTP {status}"),
    )
}

fn registry_error(path: &Path, message: impl Into<String>) -> DriverError {
    DriverError::Manifest {
        path: path.to_path_buf(),
        message: message.into(),
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    fn fixture_bundle() -> RegistryBundle {
        RegistryBundle {
            schema_version: REGISTRY_PROTOCOL_VERSION,
            package: "utility".to_owned(),
            version: "1.2.0".to_owned(),
            language: crate::CURRENT_LANGUAGE_VERSION.to_owned(),
            files: vec![
                RegistryFile {
                    path: MANIFEST_FILE.to_owned(),
                    text: format!(
                        "schema = {MANIFEST_SCHEMA_VERSION}\nlanguage = {:?}\n[module]\nname = \"utility\"\nversion = \"1.2.0\"\n",
                        crate::CURRENT_LANGUAGE_VERSION
                    ),
                },
                RegistryFile {
                    path: "lib.loom".to_owned(),
                    text: "pub fn answer() Int { 42 }\n".to_owned(),
                },
            ],
        }
    }

    #[test]
    fn registry_url_policy_requires_https_for_tokens_and_literal_loopback_for_http() {
        let manifest = Path::new("loom.toml");
        let config = |url: &str, token_env: Option<&str>| HttpRegistryConfig {
            url: url.to_owned(),
            token_env: token_env.map(str::to_owned),
        };

        assert!(
            validate_registry_config(
                manifest,
                &config("https://registry.example/v1", Some("LOOM_TOKEN"))
            )
            .is_ok()
        );
        assert!(validate_registry_config(manifest, &config("http://127.0.0.1:8080", None)).is_ok());
        assert!(validate_registry_config(manifest, &config("http://[::1]:8080", None)).is_ok());

        for rejected in [
            config("http://127.0.0.1:8080", Some("LOOM_TOKEN")),
            config("http://localhost:8080", None),
            config("http://127.0.0.1.example:8080", None),
            config("http://[::1]example:8080", None),
            config("https://user@example.test", None),
            config("https://example.test/path?query", None),
        ] {
            assert!(
                validate_registry_config(manifest, &rejected).is_err(),
                "accepted unsafe registry configuration: {}",
                rejected.url
            );
        }
    }

    #[test]
    fn bearer_token_validation_is_bounded_and_never_echoes_values() {
        assert!(validate_registry_token("LOOM_TOKEN", "0123456789abcdef").is_ok());
        for token in [
            "short".to_owned(),
            "0123456789abcde\0secret".to_owned(),
            "0123456789abcde\nsecret".to_owned(),
            "0123456789abcde\u{7f}secret".to_owned(),
            "x".repeat(MAX_AUTH_TOKEN_BYTES + 1),
        ] {
            let error = validate_registry_token("LOOM_TOKEN", &token)
                .expect_err("reject unsafe bearer token");
            assert!(error.contains("LOOM_TOKEN"), "{error}");
            assert!(!error.contains(&token), "{error}");
        }
    }

    #[test]
    fn authenticated_response_body_is_never_copied_into_diagnostics() {
        let error = http_status_error(Path::new("loom.toml"), "publish", 401).to_string();
        assert!(error.contains("HTTP 401"), "{error}");
        assert!(!error.contains("top-secret-token"), "{error}");
        assert!(!error.contains("server echoed"), "{error}");
        assert!(response_contains_secret(
            br#"{"message":"fixture\/token-is-secret"}"#,
            "fixture/token-is-secret"
        ));
    }

    #[test]
    fn bundle_validation_rejects_identity_aliases_and_reserved_paths() {
        let manifest = Path::new("loom.toml");
        assert!(validate_bundle(manifest, &fixture_bundle(), "utility", "1.2.0").is_ok());

        let invalid_paths = [
            "../src/lib.loom",
            "src/../lib.loom",
            "src//lib.loom",
            "src\\lib.loom",
            CACHE_RECORD_FILE,
            CACHED_BUNDLE_FILE,
        ];
        for path in invalid_paths {
            let mut bundle = fixture_bundle();
            bundle.files[1].path = path.to_owned();
            assert!(
                validate_bundle(manifest, &bundle, "utility", "1.2.0").is_err(),
                "accepted unsafe bundle path: {path}"
            );
        }

        let mut duplicate = fixture_bundle();
        duplicate.files.push(duplicate.files[1].clone());
        assert!(validate_bundle(manifest, &duplicate, "utility", "1.2.0").is_err());

        let mut prefix_conflict = fixture_bundle();
        prefix_conflict.files.push(RegistryFile {
            path: "lib.loom/nested".to_owned(),
            text: "not a directory".to_owned(),
        });
        assert!(validate_bundle(manifest, &prefix_conflict, "utility", "1.2.0").is_err());

        let mut identity = fixture_bundle();
        identity.package = "other".to_owned();
        assert!(validate_bundle(manifest, &identity, "utility", "1.2.0").is_err());

        let mut embedded_identity = fixture_bundle();
        embedded_identity.files[0].text = embedded_identity.files[0]
            .text
            .replace("name = \"utility\"", "name = \"other\"");
        assert!(validate_bundle(manifest, &embedded_identity, "utility", "1.2.0").is_err());
    }

    #[test]
    fn cache_validation_hashes_bundle_and_compares_materialized_contents() {
        let temporary = tempfile::tempdir().expect("temporary registry cache");
        let version_root = temporary.path().join("utility/1.2.0");
        let bundle = fixture_bundle();
        let bytes = serde_json::to_vec(&bundle).expect("encode fixture bundle");
        let sha256 = digest(&bytes);
        let manifest = materialize_bundle(
            Path::new("loom.toml"),
            &version_root,
            "https://registry.example",
            &sha256,
            &bytes,
            &bundle,
        )
        .expect("materialize valid package");
        let root = manifest.parent().expect("cached package root");
        let version = Version::parse("1.2.0").expect("fixture version");
        assert!(
            valid_cached_record(
                root,
                "https://registry.example",
                "utility",
                &version,
                Some(&sha256)
            )
            .expect("validate cache")
            .is_some()
        );

        fs::write(root.join("lib.loom"), "").expect("tamper source");
        let error = valid_cached_record(
            root,
            "https://registry.example",
            "utility",
            &version,
            Some(&sha256),
        )
        .expect_err("sidecar cannot authorize tampered materialized content")
        .to_string();
        assert!(error.contains("materialized files"), "{error}");
    }

    #[test]
    fn invalid_index_versions_are_rejected_instead_of_ignored() {
        let error = select_release(
            Path::new("loom.toml"),
            "main",
            "utility",
            &VersionReq::parse("^1").expect("requirement"),
            None,
            vec![RegistryRelease {
                version: "not-semver".to_owned(),
                sha256: "0".repeat(64),
            }],
        )
        .expect_err("invalid release version");
        assert!(error.to_string().contains("invalid version"));
    }

    #[test]
    fn registry_transport_does_not_follow_redirects() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind redirect fixture");
        let url = format!("http://{}", listener.local_addr().expect("fixture address"));
        let redirect = url.clone();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept registry request");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("request timeout");
            let mut request = [0_u8; 4096];
            let count = stream.read(&mut request).expect("read registry request");
            assert!(
                String::from_utf8_lossy(&request[..count]).starts_with("GET / HTTP/1.1"),
                "{}",
                String::from_utf8_lossy(&request[..count])
            );
            write!(
                stream,
                "HTTP/1.1 302 Found\r\nLocation: {redirect}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .expect("write redirect");
        });

        let response = http_request("GET", &url, None, None).expect("return redirect response");
        assert_eq!(response.status, 302);
        handle.join().expect("redirect fixture");
    }
}
