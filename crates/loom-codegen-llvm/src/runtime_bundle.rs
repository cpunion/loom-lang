//! Validated target-runtime bundles for explicit cross-target linking.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::emitter::native_runtime_bytes;
use crate::{CodegenError, NATIVE_RUNTIME_ABI, NativeTargetIdentity, native_target_identity};

pub const RUNTIME_BUNDLE_SCHEMA_VERSION: u32 = 1;
pub const RUNTIME_BUNDLE_MANIFEST: &str = "loom-runtime-bundle.json";

const RUNTIME_ARCHIVE_NAME: &str = "libloom_runtime.a";
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_LINKER_BYTES: u64 = 512 * 1024 * 1024;
const MAX_LINK_OUTPUT_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_BUNDLE_ENTRIES: usize = 32;
const MAX_LINK_ARGS: usize = 128;
const MAX_LINK_ARG_BYTES: usize = 512;
const MAX_TOOL_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RuntimeBundleManifest {
    schema_version: u32,
    target_triple: String,
    data_layout: String,
    runtime_abi: String,
    archive: String,
    archive_sha256: String,
    link_args: Vec<String>,
}

/// A runtime archive whose manifest and filesystem contents were fully checked.
#[derive(Clone, Debug)]
pub struct RuntimeBundle {
    root: PathBuf,
    archive: PathBuf,
    target_triple: String,
    data_layout: String,
    archive_sha256: String,
    link_args: Vec<String>,
    identity: String,
}

impl RuntimeBundle {
    /// Loads a runtime bundle and checks it against the exact LLVM target.
    ///
    /// # Errors
    ///
    /// Returns a stable error for an invalid manifest, unsafe filesystem entry,
    /// target/ABI mismatch, oversized file, or archive checksum mismatch.
    pub fn load(
        input: impl AsRef<Path>,
        expected: &NativeTargetIdentity,
    ) -> Result<Self, CodegenError> {
        let input = input.as_ref();
        let root_metadata = fs::symlink_metadata(input).map_err(|error| {
            bundle_error(format!("cannot inspect {}: {error}", input.display()))
        })?;
        if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
            return Err(bundle_error("runtime bundle must be a real directory"));
        }
        let root = fs::canonicalize(input).map_err(|error| {
            bundle_error(format!("cannot resolve {}: {error}", input.display()))
        })?;
        let manifest_path = root.join(RUNTIME_BUNDLE_MANIFEST);
        let manifest_bytes = read_bounded_regular_file(
            &manifest_path,
            MAX_MANIFEST_BYTES,
            "runtime bundle manifest",
        )?;
        let manifest = serde_json::from_slice::<RuntimeBundleManifest>(&manifest_bytes)
            .map_err(|error| bundle_error(format!("invalid manifest: {error}")))?;
        validate_manifest(&manifest, expected)?;

        let archive_relative = safe_relative_path(&manifest.archive).ok_or_else(|| {
            bundle_error("runtime archive must use one safe portable relative path")
        })?;
        if archive_relative == Path::new(RUNTIME_BUNDLE_MANIFEST) {
            return Err(bundle_error("runtime archive path is reserved"));
        }
        validate_bundle_tree(&root, &archive_relative)?;
        let archive = root.join(&archive_relative);
        let actual_archive_sha256 =
            hash_bounded_regular_file(&archive, MAX_ARCHIVE_BYTES, "runtime archive")?;
        if actual_archive_sha256 != manifest.archive_sha256 {
            return Err(CodegenError::new(
                "RuntimeBundleChecksumMismatch",
                "runtime archive SHA-256 does not match its manifest",
            ));
        }
        let manifest_sha256 = digest(&manifest_bytes);
        let identity = format!(
            "runtime-bundle-v{RUNTIME_BUNDLE_SCHEMA_VERSION};manifest-sha256={manifest_sha256};archive-sha256={actual_archive_sha256}"
        );
        Ok(Self {
            root,
            archive,
            target_triple: manifest.target_triple,
            data_layout: manifest.data_layout,
            archive_sha256: actual_archive_sha256,
            link_args: manifest.link_args,
            identity,
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn archive(&self) -> &Path {
        &self.archive
    }

    #[must_use]
    pub fn target_triple(&self) -> &str {
        &self.target_triple
    }

    #[must_use]
    pub fn data_layout(&self) -> &str {
        &self.data_layout
    }

    #[must_use]
    pub fn archive_sha256(&self) -> &str {
        &self.archive_sha256
    }

    #[must_use]
    pub fn link_args(&self) -> &[String] {
        &self.link_args
    }

    #[must_use]
    pub fn identity(&self) -> &str {
        &self.identity
    }
}

/// The result of exporting the embedded host runtime as a portable directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeBundleExport {
    pub root: PathBuf,
    pub manifest: PathBuf,
    pub archive: PathBuf,
    pub target_triple: String,
    pub data_layout: String,
    pub runtime_abi: String,
    pub archive_sha256: String,
}

/// Exports the embedded host runtime archive and an exact target manifest.
///
/// The destination must not exist. The complete bundle is validated in a
/// staging directory and atomically renamed into place.
///
/// # Errors
///
/// Returns a stable error when target discovery, staging, validation, or the
/// final atomic rename fails.
pub fn export_native_runtime_bundle(
    output: impl AsRef<Path>,
) -> Result<RuntimeBundleExport, CodegenError> {
    let output = output.as_ref();
    match fs::symlink_metadata(output) {
        Ok(_) => {
            return Err(CodegenError::new(
                "RuntimeBundleWriteFailed",
                format!("destination already exists: {}", output.display()),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(CodegenError::new(
                "RuntimeBundleWriteFailed",
                format!("cannot inspect {}: {error}", output.display()),
            ));
        }
    }
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| {
        CodegenError::new(
            "RuntimeBundleWriteFailed",
            format!("{}: {error}", parent.display()),
        )
    })?;
    let staging = tempfile::Builder::new()
        .prefix(".loom-runtime-bundle-")
        .tempdir_in(parent)
        .map_err(|error| CodegenError::new("RuntimeBundleWriteFailed", error.to_string()))?;
    let target = native_target_identity()?;
    let archive_sha256 = digest(native_runtime_bytes());
    let manifest = RuntimeBundleManifest {
        schema_version: RUNTIME_BUNDLE_SCHEMA_VERSION,
        target_triple: target.triple.clone(),
        data_layout: target.data_layout.clone(),
        runtime_abi: NATIVE_RUNTIME_ABI.to_owned(),
        archive: RUNTIME_ARCHIVE_NAME.to_owned(),
        archive_sha256: archive_sha256.clone(),
        link_args: native_runtime_link_args(),
    };
    fs::write(
        staging.path().join(RUNTIME_ARCHIVE_NAME),
        native_runtime_bytes(),
    )
    .map_err(|error| CodegenError::new("RuntimeBundleWriteFailed", error.to_string()))?;
    let mut manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| CodegenError::new("RuntimeBundleWriteFailed", error.to_string()))?;
    manifest_bytes.push(b'\n');
    fs::write(staging.path().join(RUNTIME_BUNDLE_MANIFEST), manifest_bytes)
        .map_err(|error| CodegenError::new("RuntimeBundleWriteFailed", error.to_string()))?;
    RuntimeBundle::load(staging.path(), &target)?;
    match fs::symlink_metadata(output) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => {
            return Err(CodegenError::new(
                "RuntimeBundleWriteFailed",
                format!("destination appeared during export: {}", output.display()),
            ));
        }
        Err(error) => {
            return Err(CodegenError::new(
                "RuntimeBundleWriteFailed",
                format!("cannot inspect {}: {error}", output.display()),
            ));
        }
    }
    fs::rename(staging.path(), output).map_err(|error| {
        CodegenError::new(
            "RuntimeBundleWriteFailed",
            format!("{}: {error}", output.display()),
        )
    })?;
    Ok(RuntimeBundleExport {
        root: output.to_path_buf(),
        manifest: output.join(RUNTIME_BUNDLE_MANIFEST),
        archive: output.join(RUNTIME_ARCHIVE_NAME),
        target_triple: target.triple,
        data_layout: target.data_layout,
        runtime_abi: NATIVE_RUNTIME_ABI.to_owned(),
        archive_sha256,
    })
}

/// A resolved linker whose executable bytes and version output are identified.
#[derive(Clone, Debug)]
pub struct RuntimeLinker {
    program: PathBuf,
    program_sha256: String,
    identity: String,
}

impl RuntimeLinker {
    /// Resolves and identifies an explicit linker program.
    ///
    /// # Errors
    ///
    /// Returns a stable error if the program cannot be resolved to a bounded
    /// regular file or does not return a bounded successful `--version` result.
    pub fn load(program: impl AsRef<Path>) -> Result<Self, CodegenError> {
        let program = resolve_program(program.as_ref())?;
        let program_sha256 =
            hash_bounded_regular_file(&program, MAX_LINKER_BYTES, "runtime linker executable")
                .map_err(|error| {
                    CodegenError::new("RuntimeLinkerInvalid", error.message().to_owned())
                })?;
        let output = Command::new(&program)
            .arg("--version")
            .output()
            .map_err(|error| {
                CodegenError::new(
                    "RuntimeLinkerUnavailable",
                    format!("{}: {error}", program.display()),
                )
            })?;
        if !output.status.success() {
            return Err(CodegenError::new(
                "RuntimeLinkerUnavailable",
                "explicit linker did not accept --version",
            ));
        }
        if output.stdout.len().saturating_add(output.stderr.len()) > MAX_TOOL_OUTPUT_BYTES {
            return Err(CodegenError::new(
                "RuntimeLinkerInvalid",
                "explicit linker returned oversized version output",
            ));
        }
        if output
            .stdout
            .iter()
            .chain(&output.stderr)
            .all(u8::is_ascii_whitespace)
        {
            return Err(CodegenError::new(
                "RuntimeLinkerInvalid",
                "explicit linker returned an empty version identity",
            ));
        }
        let mut version_hasher = Sha256::new();
        version_hasher.update(&output.stdout);
        version_hasher.update([0]);
        version_hasher.update(&output.stderr);
        let identity = format!(
            "path-sha256={};program-sha256={program_sha256};version-sha256={}",
            digest_path(&program),
            format_args!("{:x}", version_hasher.finalize())
        );
        Ok(Self {
            program,
            program_sha256,
            identity,
        })
    }

    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }

    #[must_use]
    pub fn identity(&self) -> &str {
        &self.identity
    }
}

/// Links an emitted target object using exactly one validated runtime bundle.
///
/// Arguments are always ordered as target object, runtime archive, declared
/// manifest arguments, then `-o STAGED_OUTPUT`. The staged output is published
/// atomically only after all inputs and the linked file are revalidated.
///
/// # Errors
///
/// Returns a stable error if an input/output path is unsafe, the linker fails,
/// or it does not produce one bounded regular output file.
pub fn link_object_with_runtime_bundle(
    object: &Path,
    output: &Path,
    bundle: &RuntimeBundle,
    linker: &RuntimeLinker,
) -> Result<(), CodegenError> {
    validate_regular_file(object, u64::MAX, "target object")?;
    verify_link_inputs(bundle, linker)?;
    match fs::symlink_metadata(output) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(CodegenError::new(
                "ArtifactWriteFailed",
                "link output must be a regular path and cannot be a symbolic link",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(CodegenError::new(
                "ArtifactWriteFailed",
                format!("{}: {error}", output.display()),
            ));
        }
    }
    if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(|error| {
            CodegenError::new(
                "ArtifactWriteFailed",
                format!("{}: {error}", parent.display()),
            )
        })?;
    }
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let staged = tempfile::Builder::new()
        .prefix(".loom-link-")
        .tempfile_in(parent)
        .map_err(|error| {
            CodegenError::new(
                "ArtifactWriteFailed",
                format!("{}: {error}", parent.display()),
            )
        })?
        .into_temp_path();
    let result = Command::new(linker.program())
        .arg(object)
        .arg(bundle.archive())
        .args(bundle.link_args())
        .arg("-o")
        .arg(&staged)
        .output()
        .map_err(|error| {
            CodegenError::new(
                "RuntimeLinkerUnavailable",
                format!("{}: {error}", linker.program().display()),
            )
        })?;
    if result.stdout.len().saturating_add(result.stderr.len()) > MAX_TOOL_OUTPUT_BYTES {
        return Err(CodegenError::new(
            "NativeLinkFailed",
            "explicit linker returned oversized output",
        ));
    }
    if !result.status.success() {
        let detail = String::from_utf8_lossy(&result.stderr);
        let detail = detail.trim();
        return Err(CodegenError::new(
            "NativeLinkFailed",
            if detail.is_empty() {
                "explicit linker failed".to_owned()
            } else {
                detail.to_owned()
            },
        ));
    }
    verify_link_inputs(bundle, linker)?;
    validate_regular_file(&staged, MAX_LINK_OUTPUT_BYTES, "linked executable")
        .map_err(|error| CodegenError::new("ArtifactWriteFailed", error.message().to_owned()))?;
    File::options()
        .read(true)
        .write(true)
        .open(&staged)
        .and_then(|file| file.sync_all())
        .map_err(|error| {
            CodegenError::new(
                "ArtifactWriteFailed",
                format!("cannot synchronize linked executable: {error}"),
            )
        })?;
    staged.persist(output).map_err(|error| {
        CodegenError::new(
            "ArtifactWriteFailed",
            format!("{}: {}", output.display(), error.error),
        )
    })
}

fn verify_link_inputs(bundle: &RuntimeBundle, linker: &RuntimeLinker) -> Result<(), CodegenError> {
    let archive_sha256 =
        hash_bounded_regular_file(bundle.archive(), MAX_ARCHIVE_BYTES, "runtime archive")?;
    if archive_sha256 != bundle.archive_sha256 {
        return Err(CodegenError::new(
            "RuntimeBundleChecksumMismatch",
            "runtime archive changed after bundle validation",
        ));
    }
    let linker_sha256 = hash_bounded_regular_file(
        linker.program(),
        MAX_LINKER_BYTES,
        "runtime linker executable",
    )
    .map_err(|error| CodegenError::new("RuntimeLinkerInvalid", error.message().to_owned()))?;
    if linker_sha256 != linker.program_sha256 {
        return Err(CodegenError::new(
            "RuntimeLinkerInvalid",
            "explicit linker changed after identity validation",
        ));
    }
    Ok(())
}

fn validate_manifest(
    manifest: &RuntimeBundleManifest,
    expected: &NativeTargetIdentity,
) -> Result<(), CodegenError> {
    if manifest.schema_version != RUNTIME_BUNDLE_SCHEMA_VERSION {
        return Err(bundle_error(format!(
            "manifest schema {} is incompatible with supported schema {RUNTIME_BUNDLE_SCHEMA_VERSION}",
            manifest.schema_version
        )));
    }
    if manifest.target_triple != expected.triple || manifest.data_layout != expected.data_layout {
        return Err(CodegenError::new(
            "RuntimeBundleTargetMismatch",
            "runtime bundle target triple/data layout does not match the emitted object",
        ));
    }
    if manifest.runtime_abi != NATIVE_RUNTIME_ABI {
        return Err(CodegenError::new(
            "RuntimeBundleAbiMismatch",
            "runtime bundle ABI does not match this compiler",
        ));
    }
    if manifest.target_triple.len() > 512
        || manifest.data_layout.len() > 8192
        || manifest.runtime_abi.len() > 512
        || !valid_digest(&manifest.archive_sha256)
    {
        return Err(bundle_error(
            "runtime bundle manifest has an invalid identity field",
        ));
    }
    validate_link_args(&manifest.link_args)
}

fn validate_link_args(arguments: &[String]) -> Result<(), CodegenError> {
    if arguments.len() > MAX_LINK_ARGS {
        return Err(bundle_error(
            "runtime bundle declares too many linker arguments",
        ));
    }
    for argument in arguments {
        let valid = !argument.is_empty()
            && argument.len() <= MAX_LINK_ARG_BYTES
            && argument.starts_with('-')
            && !argument.contains('@')
            && !argument.contains(['/', '\\'])
            && argument.bytes().all(|byte| byte.is_ascii_graphic())
            && !matches!(argument.as_str(), "-o" | "--output")
            && !argument.starts_with("-o=")
            && !argument.starts_with("--output=")
            && !argument.starts_with("-L")
            && !argument.starts_with("-F")
            && !argument.starts_with("-B")
            && !argument.starts_with("--sysroot")
            && !argument.starts_with("-isysroot")
            && !argument.contains(",-o,")
            && !argument.contains(",--output,");
        if !valid {
            return Err(bundle_error(
                "runtime bundle contains an unsafe linker argument",
            ));
        }
    }
    Ok(())
}

fn validate_bundle_tree(root: &Path, archive: &Path) -> Result<(), CodegenError> {
    let mut allowed_directories = BTreeSet::new();
    let mut parent = archive.parent();
    while let Some(directory) = parent.filter(|path| !path.as_os_str().is_empty()) {
        allowed_directories.insert(directory.to_path_buf());
        parent = directory.parent();
    }
    let mut pending = vec![root.to_path_buf()];
    let mut entries = 0_usize;
    let mut seen_manifest = false;
    let mut seen_archive = false;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)
            .map_err(|error| bundle_error(format!("cannot inspect bundle: {error}")))?
        {
            let entry = entry.map_err(|error| bundle_error(format!("invalid entry: {error}")))?;
            entries += 1;
            if entries > MAX_BUNDLE_ENTRIES {
                return Err(bundle_error("runtime bundle contains too many entries"));
            }
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|error| bundle_error(format!("cannot inspect entry: {error}")))?;
            if metadata.file_type().is_symlink() {
                return Err(bundle_error("runtime bundle cannot contain symbolic links"));
            }
            let relative = entry
                .path()
                .strip_prefix(root)
                .expect("walked runtime bundle path is below root")
                .to_path_buf();
            if metadata.is_dir() {
                if !allowed_directories.contains(&relative) {
                    return Err(bundle_error(
                        "runtime bundle contains an unexpected directory",
                    ));
                }
                pending.push(entry.path());
            } else if metadata.is_file() && relative == Path::new(RUNTIME_BUNDLE_MANIFEST) {
                seen_manifest = true;
            } else if metadata.is_file() && relative == archive {
                seen_archive = true;
            } else {
                return Err(bundle_error("runtime bundle contains an unexpected file"));
            }
        }
    }
    if !seen_manifest || !seen_archive {
        return Err(bundle_error(
            "runtime bundle must contain exactly its manifest and runtime archive",
        ));
    }
    Ok(())
}

fn safe_relative_path(value: &str) -> Option<PathBuf> {
    if value.is_empty()
        || value.len() > 1024
        || value.contains(['\\', '\0'])
        || value
            .split('/')
            .any(|component| !portable_path_component(component))
    {
        return None;
    }
    let path = Path::new(value);
    (!path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_))))
    .then(|| path.to_path_buf())
}

fn portable_path_component(component: &str) -> bool {
    if component.is_empty()
        || component.len() > 255
        || matches!(component, "." | "..")
        || component.ends_with('.')
        || !component
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
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
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

fn resolve_program(program: &Path) -> Result<PathBuf, CodegenError> {
    let candidates = if program.components().count() > 1 || program.is_absolute() {
        vec![program.to_path_buf()]
    } else {
        std::env::var_os("PATH")
            .map(|path| {
                std::env::split_paths(&path)
                    .map(|directory| directory.join(program))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let requested = candidates
        .into_iter()
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            CodegenError::new(
                "RuntimeLinkerUnavailable",
                format!("cannot resolve explicit linker `{}`", program.display()),
            )
        })?;
    let resolved = fs::canonicalize(&requested).map_err(|error| {
        CodegenError::new(
            "RuntimeLinkerUnavailable",
            format!("{}: {error}", requested.display()),
        )
    })?;
    validate_regular_file(&resolved, MAX_LINKER_BYTES, "runtime linker executable")
        .map_err(|error| CodegenError::new("RuntimeLinkerInvalid", error.message().to_owned()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let metadata = fs::metadata(&resolved)
            .map_err(|error| CodegenError::new("RuntimeLinkerInvalid", error.to_string()))?;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(CodegenError::new(
                "RuntimeLinkerInvalid",
                "explicit linker is not executable",
            ));
        }
    }
    Ok(resolved)
}

fn read_bounded_regular_file(
    path: &Path,
    maximum: u64,
    label: &str,
) -> Result<Vec<u8>, CodegenError> {
    validate_regular_file(path, maximum, label)?;
    fs::read(path).map_err(|error| bundle_error(format!("cannot read {label}: {error}")))
}

fn hash_bounded_regular_file(
    path: &Path,
    maximum: u64,
    label: &str,
) -> Result<String, CodegenError> {
    validate_regular_file(path, maximum, label)?;
    let mut file =
        File::open(path).map_err(|error| bundle_error(format!("cannot open {label}: {error}")))?;
    let mut hasher = Sha256::new();
    let mut chunk = vec![0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut chunk)
            .map_err(|error| bundle_error(format!("cannot hash {label}: {error}")))?;
        if count == 0 {
            break;
        }
        hasher.update(&chunk[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn validate_regular_file(path: &Path, maximum: u64, label: &str) -> Result<(), CodegenError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| bundle_error(format!("cannot inspect {label}: {error}")))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > maximum {
        return Err(bundle_error(format!(
            "{label} must be one bounded regular file"
        )));
    }
    Ok(())
}

fn native_runtime_link_args() -> Vec<String> {
    #[cfg(target_os = "linux")]
    {
        ["-ldl", "-lpthread", "-lm", "-lrt", "-lutil"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    }
    #[cfg(not(target_os = "linux"))]
    Vec::new()
}

fn bundle_error(message: impl Into<String>) -> CodegenError {
    CodegenError::new("RuntimeBundleInvalid", message)
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn digest_path(path: &Path) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;

        digest(path.as_os_str().as_bytes())
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;

        let mut hasher = Sha256::new();
        for unit in path.as_os_str().encode_wide() {
            hasher.update(unit.to_le_bytes());
        }
        format!("{:x}", hasher.finalize())
    }
    #[cfg(not(any(unix, windows)))]
    digest(path.to_string_lossy().as_bytes())
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
