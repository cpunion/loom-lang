//! Validated target-runtime bundles for explicit cross-target linking.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, SyncSender};
use std::thread::{self, JoinHandle};

// Rust 1.88 does not expose Windows volume/file-index metadata on stable Rust.
// `same-file` supplies the cross-platform opened-handle identity needed to
// close lstat/open replacement races without adding unsafe code here.
use same_file::Handle;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::native_artifact::native_runtime_archive_name;
use crate::native_link::{linker_version_arguments, native_link_command, native_runtime_link_args};
use crate::{CodegenError, NATIVE_RUNTIME_ABI, NativeTargetIdentity, native_target_identity};

pub const RUNTIME_BUNDLE_SCHEMA_VERSION: u32 = 2;
pub const RUNTIME_BUNDLE_MANIFEST: &str = "loom-runtime-bundle.json";
pub const RUNTIME_CPU: &str = "generic";
pub const RUNTIME_CPU_FEATURES: &str = "";

const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_LINKER_BYTES: u64 = 512 * 1024 * 1024;
const MAX_LINK_OUTPUT_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_BUNDLE_ENTRIES: usize = 32;
const MAX_TOOL_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RuntimeBundleManifest {
    schema_version: u32,
    target_triple: String,
    data_layout: String,
    runtime_cpu: String,
    runtime_cpu_features: String,
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
    runtime_cpu: String,
    runtime_cpu_features: String,
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
        let root = canonical_real_directory(input, "runtime bundle")?;
        let manifest_path = root.join(RUNTIME_BUNDLE_MANIFEST);
        let manifest_bytes = read_bounded_regular_file(
            &manifest_path,
            MAX_MANIFEST_BYTES,
            "runtime bundle manifest",
        )?;
        let manifest = serde_json::from_slice::<RuntimeBundleManifest>(&manifest_bytes)
            .map_err(|error| bundle_error(format!("invalid manifest: {error}")))?;
        validate_manifest(&manifest, expected)?;

        let archive_name = safe_portable_file_name(&manifest.archive)
            .ok_or_else(|| bundle_error("runtime archive must use one safe portable filename"))?;
        if archive_name == RUNTIME_BUNDLE_MANIFEST {
            return Err(bundle_error("runtime archive path is reserved"));
        }
        validate_bundle_tree(&root, archive_name)?;
        let archive = root.join(archive_name);
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
            runtime_cpu: manifest.runtime_cpu,
            runtime_cpu_features: manifest.runtime_cpu_features,
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
    pub fn runtime_cpu(&self) -> &str {
        &self.runtime_cpu
    }

    #[must_use]
    pub fn runtime_cpu_features(&self) -> &str {
        &self.runtime_cpu_features
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

/// The result of packing one explicit host runtime archive as a portable directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackedRuntimeBundle {
    pub root: PathBuf,
    pub manifest: PathBuf,
    pub archive: PathBuf,
    pub target_triple: String,
    pub data_layout: String,
    pub runtime_cpu: String,
    pub runtime_cpu_features: String,
    pub runtime_abi: String,
    pub archive_sha256: String,
}

/// Packs one explicit host runtime archive with an exact target manifest.
///
/// The archive must be one bounded regular file and cannot be a symbolic link.
/// Its bytes are copied to the canonical target archive name. The destination
/// must not exist; the complete bundle is loaded and validated in a staging
/// directory, atomically renamed into place, and loaded again at its final path.
///
/// # Errors
///
/// Returns a stable error when target discovery, staging, validation, or the
/// final atomic rename fails.
pub fn pack_native_runtime_bundle(
    archive: impl AsRef<Path>,
    output: impl AsRef<Path>,
) -> Result<PackedRuntimeBundle, CodegenError> {
    let archive = archive.as_ref();
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
    let archive_name = native_runtime_archive_name(Some(&target.triple)).to_owned();
    let staged_archive = staging.path().join(&archive_name);
    let archive_sha256 = copy_bounded_regular_file(
        archive,
        &staged_archive,
        MAX_ARCHIVE_BYTES,
        "runtime archive",
    )?;
    let manifest = RuntimeBundleManifest {
        schema_version: RUNTIME_BUNDLE_SCHEMA_VERSION,
        target_triple: target.triple.clone(),
        data_layout: target.data_layout.clone(),
        runtime_cpu: RUNTIME_CPU.to_owned(),
        runtime_cpu_features: RUNTIME_CPU_FEATURES.to_owned(),
        runtime_abi: NATIVE_RUNTIME_ABI.to_owned(),
        archive: archive_name.clone(),
        archive_sha256: archive_sha256.clone(),
        link_args: native_runtime_link_args(&target.triple),
    };
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
                format!("destination appeared during packing: {}", output.display()),
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
    let packed = RuntimeBundle::load(output, &target)?;
    Ok(PackedRuntimeBundle {
        root: packed.root().to_path_buf(),
        manifest: packed.root().join(RUNTIME_BUNDLE_MANIFEST),
        archive: packed.archive().to_path_buf(),
        target_triple: packed.target_triple().to_owned(),
        data_layout: packed.data_layout().to_owned(),
        runtime_cpu: packed.runtime_cpu().to_owned(),
        runtime_cpu_features: packed.runtime_cpu_features().to_owned(),
        runtime_abi: NATIVE_RUNTIME_ABI.to_owned(),
        archive_sha256: packed.archive_sha256().to_owned(),
    })
}

/// A resolved linker whose executable bytes and version behavior are validated.
#[derive(Clone, Debug)]
pub struct RuntimeLinker {
    program: PathBuf,
    program_identity: PathBuf,
    program_sha256: String,
}

struct RemoveFileOnDrop(Option<PathBuf>);

impl RemoveFileOnDrop {
    fn new(path: Option<PathBuf>) -> Self {
        Self(path)
    }
}

impl Drop for RemoveFileOnDrop {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

impl RuntimeLinker {
    /// Resolves and identifies the selected linker program.
    ///
    /// # Errors
    ///
    /// Returns a stable error if the program cannot be resolved to a bounded
    /// regular file or does not return a bounded successful `--version` result.
    pub fn load(program: impl AsRef<Path>) -> Result<Self, CodegenError> {
        let (program, program_identity) = resolve_program(program.as_ref())?;
        let program_sha256 = hash_bounded_regular_file(
            &program_identity,
            MAX_LINKER_BYTES,
            "runtime linker executable",
        )
        .map_err(|error| CodegenError::new("RuntimeLinkerInvalid", error.message().to_owned()))?;
        let mut probe = Command::new(&program);
        probe.args(linker_version_arguments(&program));
        let output = run_bounded_command(&mut probe, MAX_TOOL_OUTPUT_BYTES).map_err(|error| {
            command_error(
                error,
                "RuntimeLinkerUnavailable",
                "RuntimeLinkerInvalid",
                &format!("{}", program.display()),
                "selected linker returned oversized version output",
            )
        })?;
        if !output.status.success() {
            return Err(CodegenError::new(
                "RuntimeLinkerUnavailable",
                "selected linker did not accept its version/help probe",
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
                "selected linker returned an empty version identity",
            ));
        }
        Ok(Self {
            program,
            program_identity,
            program_sha256,
        })
    }

    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }
}

/// Links an emitted target object using exactly one validated runtime bundle.
///
/// Arguments are ordered by the detected linker driver convention. The staged
/// executable (and an MSVC PDB) are validated before publication. Publishing
/// the two final paths is intentionally not described as filesystem-atomic.
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
    match fs::symlink_metadata(output) {
        Ok(metadata) if metadata_is_link_like(&metadata) || !metadata.is_file() => {
            return Err(CodegenError::new(
                "ArtifactWriteFailed",
                "link output must be a regular path and cannot be a symbolic link or reparse point",
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
    let runtime_snapshot = snapshot_runtime_archive(bundle, parent)?;
    verify_linker(linker)?;
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
    let link = native_link_command(
        linker.program(),
        bundle.target_triple(),
        object,
        &[runtime_snapshot.path()],
        bundle.link_args(),
        &staged,
    );
    let _staged_pdb_cleanup = RemoveFileOnDrop::new(link.pdb.clone());
    let mut command = Command::new(linker.program());
    command.args(&link.arguments);
    let result = run_bounded_command(&mut command, MAX_TOOL_OUTPUT_BYTES).map_err(|error| {
        command_error(
            error,
            "RuntimeLinkerUnavailable",
            "NativeLinkFailed",
            &format!("{}", linker.program().display()),
            "selected linker returned oversized output",
        )
    })?;
    if !result.status.success() {
        return Err(CodegenError::new(
            "NativeLinkFailed",
            failed_command_detail(&result.stdout, &result.stderr, "selected linker failed"),
        ));
    }
    verify_runtime_snapshot(&runtime_snapshot, bundle.archive_sha256())?;
    verify_linker(linker)?;
    validate_regular_file(&staged, MAX_LINK_OUTPUT_BYTES, "linked executable")
        .map_err(|error| CodegenError::new("ArtifactWriteFailed", error.message().to_owned()))?;
    validate_staged_pdb(link.pdb.as_deref())?;
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
    })?;
    publish_staged_pdb(link.pdb.as_deref(), output, Some(bundle.target_triple()))?;
    Ok(())
}

fn validate_staged_pdb(pdb: Option<&Path>) -> Result<(), CodegenError> {
    if let Some(pdb) = pdb {
        validate_regular_file(pdb, MAX_LINK_OUTPUT_BYTES, "linked PDB").map_err(|error| {
            CodegenError::new("DebugInfoWriteFailed", error.message().to_owned())
        })?;
    }
    Ok(())
}

fn publish_staged_pdb(
    pdb: Option<&Path>,
    output: &Path,
    target_triple: Option<&str>,
) -> Result<(), CodegenError> {
    let Some(pdb) = pdb else {
        return Ok(());
    };
    let destination = crate::native_artifact_path(
        output,
        target_triple,
        crate::NativeArtifactKind::DebugDatabase,
    );
    match fs::symlink_metadata(&destination) {
        Ok(metadata) if metadata_is_link_like(&metadata) || !metadata.is_file() => {
            return Err(CodegenError::new(
                "DebugInfoWriteFailed",
                "PDB output must be a regular path and cannot be a symbolic link or reparse point",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(CodegenError::new(
                "DebugInfoWriteFailed",
                format!("{}: {error}", destination.display()),
            ));
        }
    }
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut publication = tempfile::Builder::new()
        .prefix(".loom-pdb-publish-")
        .suffix(".pdb")
        .tempfile_in(parent)
        .map_err(|error| CodegenError::new("DebugInfoWriteFailed", error.to_string()))?;
    copy_bounded_regular_file_into(
        pdb,
        publication.as_file_mut(),
        MAX_LINK_OUTPUT_BYTES,
        "linked PDB",
    )
    .map_err(|error| CodegenError::new("DebugInfoWriteFailed", error.message().to_owned()))?;
    publication
        .as_file()
        .sync_all()
        .map_err(|error| CodegenError::new("DebugInfoWriteFailed", error.to_string()))?;
    publication.persist(&destination).map_err(|error| {
        CodegenError::new(
            "DebugInfoWriteFailed",
            format!("{}: {}", destination.display(), error.error),
        )
    })?;
    Ok(())
}

fn snapshot_runtime_archive(
    bundle: &RuntimeBundle,
    parent: &Path,
) -> Result<tempfile::NamedTempFile, CodegenError> {
    let suffix = bundle
        .archive()
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .map_or_else(String::new, |extension| format!(".{extension}"));
    let mut snapshot = tempfile::Builder::new()
        .prefix(".loom-runtime-link-")
        .suffix(&suffix)
        .tempfile_in(parent)
        .map_err(|error| CodegenError::new("ArtifactWriteFailed", error.to_string()))?;
    let archive_sha256 = copy_bounded_regular_file_into(
        bundle.archive(),
        snapshot.as_file_mut(),
        MAX_ARCHIVE_BYTES,
        "runtime archive",
    )
    .map_err(|error| {
        if error.code() == "RuntimeBundleWriteFailed" {
            CodegenError::new("ArtifactWriteFailed", error.message().to_owned())
        } else {
            error
        }
    })?;
    if archive_sha256 != bundle.archive_sha256 {
        return Err(CodegenError::new(
            "RuntimeBundleChecksumMismatch",
            "runtime archive changed after bundle validation",
        ));
    }
    snapshot
        .as_file()
        .sync_all()
        .map_err(|error| CodegenError::new("ArtifactWriteFailed", error.to_string()))?;
    Ok(snapshot)
}

fn verify_runtime_snapshot(
    snapshot: &tempfile::NamedTempFile,
    expected_sha256: &str,
) -> Result<(), CodegenError> {
    let actual = hash_bounded_file_handle(
        snapshot.as_file(),
        MAX_ARCHIVE_BYTES,
        "runtime archive snapshot",
    )?;
    if actual != expected_sha256 {
        return Err(CodegenError::new(
            "RuntimeBundleChecksumMismatch",
            "private runtime archive snapshot changed during linking",
        ));
    }
    Ok(())
}

fn verify_linker(linker: &RuntimeLinker) -> Result<(), CodegenError> {
    let current_identity = fs::canonicalize(linker.program()).map_err(|error| {
        CodegenError::new(
            "RuntimeLinkerInvalid",
            format!("{}: {error}", linker.program().display()),
        )
    })?;
    if current_identity != linker.program_identity {
        return Err(CodegenError::new(
            "RuntimeLinkerInvalid",
            "selected linker alias changed after identity validation",
        ));
    }
    let linker_sha256 = hash_bounded_regular_file(
        &linker.program_identity,
        MAX_LINKER_BYTES,
        "runtime linker executable",
    )
    .map_err(|error| CodegenError::new("RuntimeLinkerInvalid", error.message().to_owned()))?;
    if linker_sha256 != linker.program_sha256 {
        return Err(CodegenError::new(
            "RuntimeLinkerInvalid",
            "selected linker changed after identity validation",
        ));
    }
    Ok(())
}

#[derive(Debug)]
enum BoundedCommandError {
    Io(std::io::Error),
    OutputLimit,
}

#[derive(Clone, Copy, Debug)]
enum CommandStream {
    Stdout,
    Stderr,
}

#[derive(Debug)]
enum DrainMessage {
    Bytes(CommandStream, Vec<u8>),
    Done,
    Error(std::io::Error),
}

fn command_error(
    error: BoundedCommandError,
    io_code: &'static str,
    limit_code: &'static str,
    program: &str,
    limit_message: &'static str,
) -> CodegenError {
    match error {
        BoundedCommandError::Io(error) => CodegenError::new(io_code, format!("{program}: {error}")),
        BoundedCommandError::OutputLimit => CodegenError::new(limit_code, limit_message),
    }
}

fn failed_command_detail(stdout: &[u8], stderr: &[u8], fallback: &str) -> String {
    let stdout = String::from_utf8_lossy(stdout);
    let stdout = stdout.trim();
    let stderr = String::from_utf8_lossy(stderr);
    let stderr = stderr.trim();
    match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => fallback.to_owned(),
        (false, true) => stdout.to_owned(),
        (true, false) => stderr.to_owned(),
        (false, false) => format!("{stdout}\n{stderr}"),
    }
}

fn run_bounded_command(
    command: &mut Command,
    maximum: usize,
) -> Result<std::process::Output, BoundedCommandError> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(BoundedCommandError::Io)?;
    let Some(stdout) = child.stdout.take() else {
        terminate_and_wait(&mut child);
        return Err(BoundedCommandError::Io(std::io::Error::other(
            "cannot capture linker stdout",
        )));
    };
    let Some(stderr) = child.stderr.take() else {
        terminate_and_wait(&mut child);
        return Err(BoundedCommandError::Io(std::io::Error::other(
            "cannot capture linker stderr",
        )));
    };

    // The bounded synchronous queue applies backpressure before either drain
    // can allocate more than a few fixed-size chunks beyond the result limit.
    let (sender, receiver) = mpsc::sync_channel(4);
    let drains = [
        spawn_command_drain(stdout, CommandStream::Stdout, sender.clone()),
        spawn_command_drain(stderr, CommandStream::Stderr, sender),
    ];
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut total = 0_usize;
    let mut completed = 0_u8;
    while completed < 2 {
        match receiver.recv() {
            Ok(DrainMessage::Bytes(stream, bytes)) => {
                if bytes.len() > maximum.saturating_sub(total) {
                    terminate_and_wait(&mut child);
                    drop(receiver);
                    drop(drains);
                    return Err(BoundedCommandError::OutputLimit);
                }
                total += bytes.len();
                match stream {
                    CommandStream::Stdout => stdout.extend_from_slice(&bytes),
                    CommandStream::Stderr => stderr.extend_from_slice(&bytes),
                }
            }
            Ok(DrainMessage::Done) => completed += 1,
            Ok(DrainMessage::Error(error)) => {
                terminate_and_wait(&mut child);
                drop(receiver);
                drop(drains);
                return Err(BoundedCommandError::Io(error));
            }
            Err(error) => {
                terminate_and_wait(&mut child);
                drop(drains);
                return Err(BoundedCommandError::Io(std::io::Error::other(error)));
            }
        }
    }
    let status = child.wait().map_err(BoundedCommandError::Io)?;
    for drain in drains {
        drain.join().map_err(|_| {
            BoundedCommandError::Io(std::io::Error::other("linker output drain panicked"))
        })?;
    }
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

fn spawn_command_drain(
    mut reader: impl Read + Send + 'static,
    stream: CommandStream,
    sender: SyncSender<DrainMessage>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut chunk = [0_u8; 16 * 1024];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => {
                    let _ = sender.send(DrainMessage::Done);
                    return;
                }
                Ok(count) => {
                    if sender
                        .send(DrainMessage::Bytes(stream, chunk[..count].to_vec()))
                        .is_err()
                    {
                        return;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => {
                    let _ = sender.send(DrainMessage::Error(error));
                    return;
                }
            }
        }
    })
}

fn terminate_and_wait(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
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
    if manifest.runtime_cpu != RUNTIME_CPU || manifest.runtime_cpu_features != RUNTIME_CPU_FEATURES
    {
        return Err(CodegenError::new(
            "RuntimeBundleTargetMismatch",
            "runtime bundle must use the portable generic CPU policy without extra features",
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
        || manifest.runtime_cpu.len() > 512
        || manifest.runtime_cpu_features.len() > 8192
        || manifest.runtime_abi.len() > 512
        || !valid_digest(&manifest.archive_sha256)
    {
        return Err(bundle_error(
            "runtime bundle manifest has an invalid identity field",
        ));
    }
    if manifest.link_args != native_runtime_link_args(&expected.triple) {
        return Err(bundle_error(
            "runtime bundle linker arguments do not match the compiler-derived target closure",
        ));
    }
    Ok(())
}

fn validate_bundle_tree(root: &Path, archive: &str) -> Result<(), CodegenError> {
    let mut entries = 0_usize;
    let mut seen_manifest = false;
    let mut seen_archive = false;
    for entry in fs::read_dir(root)
        .map_err(|error| bundle_error(format!("cannot inspect bundle: {error}")))?
    {
        let entry = entry.map_err(|error| bundle_error(format!("invalid entry: {error}")))?;
        entries += 1;
        if entries > MAX_BUNDLE_ENTRIES {
            return Err(bundle_error("runtime bundle contains too many entries"));
        }
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| bundle_error(format!("cannot inspect entry: {error}")))?;
        if metadata_is_link_like(&metadata) {
            return Err(bundle_error(
                "runtime bundle cannot contain symbolic links or reparse points",
            ));
        }
        let file_name = entry.file_name();
        if metadata.is_file() && file_name == RUNTIME_BUNDLE_MANIFEST {
            seen_manifest = true;
        } else if metadata.is_file() && file_name == archive {
            seen_archive = true;
        } else {
            return Err(bundle_error(
                "runtime bundle may contain only its root manifest and runtime archive",
            ));
        }
    }
    if !seen_manifest || !seen_archive {
        return Err(bundle_error(
            "runtime bundle must contain exactly its manifest and runtime archive",
        ));
    }
    Ok(())
}

fn safe_portable_file_name(value: &str) -> Option<&str> {
    portable_path_component(value).then_some(value)
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

fn resolve_program(program: &Path) -> Result<(PathBuf, PathBuf), CodegenError> {
    let candidates = if program.components().count() > 1 || program.is_absolute() {
        program_path_candidates(program.to_path_buf())
    } else {
        std::env::var_os("PATH")
            .map(|path| {
                std::env::split_paths(&path)
                    .flat_map(|directory| program_path_candidates(directory.join(program)))
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
                format!("cannot resolve selected linker `{}`", program.display()),
            )
        })?;
    let file_name = requested.file_name().ok_or_else(|| {
        CodegenError::new(
            "RuntimeLinkerUnavailable",
            format!("cannot resolve selected linker `{}`", program.display()),
        )
    })?;
    let parent = requested
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let invocation_parent = fs::canonicalize(parent).map_err(|error| {
        CodegenError::new(
            "RuntimeLinkerUnavailable",
            format!("{}: {error}", parent.display()),
        )
    })?;
    let invocation = invocation_parent.join(file_name);
    let identity = fs::canonicalize(&invocation).map_err(|error| {
        CodegenError::new(
            "RuntimeLinkerUnavailable",
            format!("{}: {error}", invocation.display()),
        )
    })?;
    validate_regular_file(&identity, MAX_LINKER_BYTES, "runtime linker executable")
        .map_err(|error| CodegenError::new("RuntimeLinkerInvalid", error.message().to_owned()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let metadata = fs::metadata(&identity)
            .map_err(|error| CodegenError::new("RuntimeLinkerInvalid", error.to_string()))?;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(CodegenError::new(
                "RuntimeLinkerInvalid",
                "selected linker is not executable",
            ));
        }
    }
    Ok((invocation, identity))
}

fn program_path_candidates(program: PathBuf) -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        if program.extension().is_none() {
            return vec![program.clone(), program.with_extension("exe")];
        }
    }
    vec![program]
}

fn copy_bounded_regular_file(
    source: &Path,
    destination: &Path,
    maximum: u64,
    label: &str,
) -> Result<String, CodegenError> {
    let mut destination_file = File::options()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|error| CodegenError::new("RuntimeBundleWriteFailed", error.to_string()))?;
    let digest = copy_bounded_regular_file_into(source, &mut destination_file, maximum, label)?;
    destination_file
        .sync_all()
        .map_err(|error| CodegenError::new("RuntimeBundleWriteFailed", error.to_string()))?;
    Ok(digest)
}

fn copy_bounded_regular_file_into(
    source: &Path,
    destination: &mut File,
    maximum: u64,
    label: &str,
) -> Result<String, CodegenError> {
    let mut source_file = open_bounded_regular_file(source, maximum, label)?;
    destination
        .set_len(0)
        .map_err(|error| CodegenError::new("RuntimeBundleWriteFailed", error.to_string()))?;
    destination
        .seek(SeekFrom::Start(0))
        .map_err(|error| CodegenError::new("RuntimeBundleWriteFailed", error.to_string()))?;
    let mut hasher = Sha256::new();
    read_bounded(&mut source_file, maximum, label, |chunk| {
        hasher.update(chunk);
        destination
            .write_all(chunk)
            .map_err(|error| CodegenError::new("RuntimeBundleWriteFailed", error.to_string()))?;
        Ok(())
    })?;
    Ok(format!("{:x}", hasher.finalize()))
}

fn read_bounded_regular_file(
    path: &Path,
    maximum: u64,
    label: &str,
) -> Result<Vec<u8>, CodegenError> {
    let mut file = open_bounded_regular_file(path, maximum, label)?;
    let capacity = file
        .metadata()
        .ok()
        .and_then(|metadata| usize::try_from(metadata.len()).ok())
        .unwrap_or(0);
    let mut bytes = Vec::with_capacity(capacity);
    read_bounded(&mut file, maximum, label, |chunk| {
        bytes.extend_from_slice(chunk);
        Ok(())
    })?;
    Ok(bytes)
}

fn hash_bounded_regular_file(
    path: &Path,
    maximum: u64,
    label: &str,
) -> Result<String, CodegenError> {
    let file = open_bounded_regular_file(path, maximum, label)?;
    hash_bounded_file_handle(&file, maximum, label)
}

fn hash_bounded_file_handle(
    file: &File,
    maximum: u64,
    label: &str,
) -> Result<String, CodegenError> {
    let metadata = file
        .metadata()
        .map_err(|error| bundle_error(format!("cannot inspect opened {label}: {error}")))?;
    validate_regular_metadata(&metadata, maximum, label)?;
    let mut file = file
        .try_clone()
        .map_err(|error| bundle_error(format!("cannot clone opened {label}: {error}")))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| bundle_error(format!("cannot rewind opened {label}: {error}")))?;
    let mut hasher = Sha256::new();
    read_bounded(&mut file, maximum, label, |chunk| {
        hasher.update(chunk);
        Ok(())
    })?;
    Ok(format!("{:x}", hasher.finalize()))
}

fn validate_regular_file(path: &Path, maximum: u64, label: &str) -> Result<(), CodegenError> {
    open_bounded_regular_file(path, maximum, label).map(|_| ())
}

fn open_bounded_regular_file(path: &Path, maximum: u64, label: &str) -> Result<File, CodegenError> {
    let before = fs::symlink_metadata(path)
        .map_err(|error| bundle_error(format!("cannot inspect {label}: {error}")))?;
    validate_regular_metadata(&before, maximum, label)?;
    let before_handle = Handle::from_path(path)
        .map_err(|error| bundle_error(format!("cannot identify {label}: {error}")))?;
    let file =
        File::open(path).map_err(|error| bundle_error(format!("cannot open {label}: {error}")))?;
    let opened = file
        .metadata()
        .map_err(|error| bundle_error(format!("cannot inspect opened {label}: {error}")))?;
    validate_regular_metadata(&opened, maximum, label)?;
    let opened_handle = Handle::from_file(
        file.try_clone()
            .map_err(|error| bundle_error(format!("cannot clone opened {label}: {error}")))?,
    )
    .map_err(|error| bundle_error(format!("cannot identify opened {label}: {error}")))?;
    let after = fs::symlink_metadata(path)
        .map_err(|error| bundle_error(format!("cannot re-inspect {label}: {error}")))?;
    validate_regular_metadata(&after, maximum, label)?;
    let after_handle = Handle::from_path(path)
        .map_err(|error| bundle_error(format!("cannot re-identify {label}: {error}")))?;
    if before_handle != opened_handle || after_handle != opened_handle {
        return Err(bundle_error(format!(
            "{label} changed while it was being opened"
        )));
    }
    Ok(file)
}

fn validate_regular_metadata(
    metadata: &fs::Metadata,
    maximum: u64,
    label: &str,
) -> Result<(), CodegenError> {
    if metadata_is_link_like(metadata) || !metadata.is_file() || metadata.len() > maximum {
        return Err(bundle_error(format!(
            "{label} must be one bounded regular file"
        )));
    }
    Ok(())
}

fn read_bounded(
    reader: &mut File,
    maximum: u64,
    label: &str,
    mut consume: impl FnMut(&[u8]) -> Result<(), CodegenError>,
) -> Result<(), CodegenError> {
    let mut total = 0_u64;
    let mut chunk = vec![0_u8; 64 * 1024];
    loop {
        let count = reader
            .read(&mut chunk)
            .map_err(|error| bundle_error(format!("cannot read {label}: {error}")))?;
        if count == 0 {
            return Ok(());
        }
        total = total.saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
        if total > maximum {
            return Err(bundle_error(format!(
                "{label} must be one bounded regular file"
            )));
        }
        consume(&chunk[..count])?;
    }
}

fn canonical_real_directory(path: &Path, label: &str) -> Result<PathBuf, CodegenError> {
    let before = fs::symlink_metadata(path)
        .map_err(|error| bundle_error(format!("cannot inspect {label}: {error}")))?;
    validate_real_directory_metadata(&before, label)?;
    let before_handle = Handle::from_path(path)
        .map_err(|error| bundle_error(format!("cannot identify {label}: {error}")))?;
    let canonical = fs::canonicalize(path)
        .map_err(|error| bundle_error(format!("cannot resolve {}: {error}", path.display())))?;
    let resolved = fs::symlink_metadata(&canonical)
        .map_err(|error| bundle_error(format!("cannot inspect resolved {label}: {error}")))?;
    validate_real_directory_metadata(&resolved, label)?;
    let after = fs::symlink_metadata(path)
        .map_err(|error| bundle_error(format!("cannot re-inspect {label}: {error}")))?;
    validate_real_directory_metadata(&after, label)?;
    let resolved_handle = Handle::from_path(&canonical)
        .map_err(|error| bundle_error(format!("cannot identify resolved {label}: {error}")))?;
    let after_handle = Handle::from_path(path)
        .map_err(|error| bundle_error(format!("cannot re-identify {label}: {error}")))?;
    if before_handle != resolved_handle || after_handle != resolved_handle {
        return Err(bundle_error(format!(
            "{label} changed while it was being resolved"
        )));
    }
    Ok(canonical)
}

fn validate_real_directory_metadata(
    metadata: &fs::Metadata,
    label: &str,
) -> Result<(), CodegenError> {
    if metadata_is_link_like(metadata) || !metadata.is_dir() {
        return Err(bundle_error(format!("{label} must be a real directory")));
    }
    Ok(())
}

fn metadata_is_link_like(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    false
}

fn bundle_error(message: impl Into<String>) -> CodegenError {
    CodegenError::new("RuntimeBundleInvalid", message)
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_linker_detail_preserves_diagnostics_from_both_streams() {
        assert_eq!(
            failed_command_detail(
                b"LINK : fatal error LNK1104: cannot open file 'example.lib'\r\n",
                b"clang: error: linker command failed\n",
                "fallback",
            ),
            "LINK : fatal error LNK1104: cannot open file 'example.lib'\n\
             clang: error: linker command failed"
        );
        assert_eq!(failed_command_detail(b"", b"", "fallback"), "fallback");
    }

    #[test]
    fn staged_msvc_pdb_is_published_to_the_final_companion_path() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let staged = directory.path().join(".loom-link-staged.pdb");
        let output = directory.path().join("program.exe");
        let destination = directory.path().join("program.pdb");
        fs::write(&staged, b"validated PDB bytes").expect("write staged PDB");
        fs::write(&destination, b"previous PDB bytes").expect("write previous PDB");

        publish_staged_pdb(Some(&staged), &output, Some("x86_64-pc-windows-msvc"))
            .expect("publish PDB");

        assert_eq!(
            fs::read(destination).expect("read published PDB"),
            b"validated PDB bytes"
        );
        assert!(
            fs::read_dir(directory.path())
                .expect("read publication directory")
                .all(|entry| !entry
                    .expect("valid publication entry")
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".loom-pdb-publish-")),
            "atomic PDB publication must clean its private staging file"
        );
    }

    #[test]
    fn opened_file_reads_enforce_the_byte_limit_instead_of_trusting_metadata() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("growing-input");
        fs::write(&path, b"five!").expect("write bounded input");
        let mut file = File::open(path).expect("open input");
        let mut observed = Vec::new();

        let error = read_bounded(&mut file, 4, "growing input", |chunk| {
            observed.extend_from_slice(chunk);
            Ok(())
        })
        .expect_err("enforce the actual stream byte count");

        assert_eq!(error.code(), "RuntimeBundleInvalid");
        assert!(observed.is_empty(), "oversized chunk is never consumed");
    }

    #[cfg(unix)]
    #[test]
    fn bounded_command_stops_an_unending_combined_output_stream() {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "while :; do printf '12345678'; printf 'abcdefgh' >&2; done",
        ]);

        let error = run_bounded_command(&mut command, 1024)
            .expect_err("terminate a process as soon as combined output exceeds the bound");

        assert!(matches!(error, BoundedCommandError::OutputLimit));
    }

    #[cfg(unix)]
    #[test]
    fn clang_cl_symlink_alias_preserves_its_driver_flavor() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let directory = tempfile::tempdir().expect("temporary directory");
        let driver = directory.path().join("clang-19");
        let alias = directory.path().join("clang-cl");
        fs::write(&driver, "#!/bin/sh\nprintf 'test clang driver v1\\n'\n")
            .expect("write test driver");
        let mut permissions = fs::metadata(&driver)
            .expect("driver metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&driver, permissions).expect("make driver executable");
        symlink(&driver, &alias).expect("create clang-cl alias");

        let linker = RuntimeLinker::load(&alias).expect("load clang-cl alias");
        let command = native_link_command(
            linker.program(),
            "x86_64-pc-windows-msvc",
            Path::new("program.obj"),
            &[Path::new("loom_runtime.lib")],
            &[],
            Path::new("program.exe"),
        );

        assert_eq!(
            linker.program().file_name(),
            Some(std::ffi::OsStr::new("clang-cl"))
        );
        assert!(
            command
                .arguments
                .iter()
                .any(|argument| argument == "/Feprogram.exe"),
            "the invocation alias must select clang-cl argument syntax"
        );
    }

    #[cfg(unix)]
    #[test]
    fn target_prefixed_symlink_alias_is_used_for_probe_and_link_execution() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let directory = tempfile::tempdir().expect("temporary directory");
        let driver = directory.path().join("clang-19");
        let alias = directory.path().join("aarch64-linux-gnu-clang");
        fs::write(
            &driver,
            r#"#!/bin/sh
set -eu
directory=$(dirname "$0")
invocation=$(basename "$0")
if [ "${1-}" = "--version" ]; then
    printf '%s' "$invocation" > "$directory/probe-marker"
    printf 'test target-prefixed driver v1\n'
    exit 0
fi
printf '%s' "$invocation" > "$directory/link-marker"
while [ "$#" -gt 0 ]; do
    if [ "$1" = "-o" ]; then
        shift
        printf 'linked output' > "$1"
        exit 0
    fi
    shift
done
exit 9
"#,
        )
        .expect("write test driver");
        let mut permissions = fs::metadata(&driver)
            .expect("driver metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&driver, permissions).expect("make driver executable");
        symlink(&driver, &alias).expect("create target-prefixed alias");

        let linker = RuntimeLinker::load(&alias).expect("load target-prefixed alias");
        assert_eq!(
            fs::read_to_string(directory.path().join("probe-marker")).expect("read probe marker"),
            "aarch64-linux-gnu-clang"
        );

        let bundle_path = directory.path().join("runtime");
        let archive = directory.path().join("input-runtime.a");
        fs::write(&archive, b"test runtime archive").expect("write runtime archive");
        pack_native_runtime_bundle(&archive, &bundle_path).expect("pack runtime bundle");
        let target = native_target_identity().expect("host target identity");
        let bundle = RuntimeBundle::load(&bundle_path, &target).expect("load runtime bundle");
        let object = directory.path().join("program.o");
        let output = directory.path().join("program");
        fs::write(&object, b"test object").expect("write object");

        link_object_with_runtime_bundle(&object, &output, &bundle, &linker)
            .expect("link through target-prefixed alias");

        assert_eq!(
            fs::read_to_string(directory.path().join("link-marker")).expect("read link marker"),
            "aarch64-linux-gnu-clang"
        );
        assert_eq!(
            fs::read(output).expect("read linked output"),
            b"linked output"
        );
    }

    #[test]
    fn staged_msvc_pdb_never_overwrites_an_executable_named_like_a_pdb() {
        let directory = tempfile::tempdir().expect("temporary directory");
        for output_name in ["program.pdb", "program.PDB"] {
            let staged = directory.path().join(format!(".{output_name}.staged"));
            let output = directory.path().join(output_name);
            let companion = directory.path().join(format!("{output_name}.pdb"));
            fs::write(&staged, b"validated PDB bytes").expect("write staged PDB");
            fs::write(&output, b"preserved executable bytes").expect("write executable");

            publish_staged_pdb(Some(&staged), &output, Some("x86_64-pc-windows-msvc"))
                .expect("publish independent PDB companion");

            assert_eq!(
                fs::read(&output).expect("read preserved executable"),
                b"preserved executable bytes"
            );
            assert_eq!(
                fs::read(companion).expect("read independent PDB companion"),
                b"validated PDB bytes"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn staged_msvc_pdb_rejects_a_symlink_destination() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("temporary directory");
        let staged = directory.path().join(".loom-link-staged.pdb");
        let outside = directory.path().join("outside");
        let output = directory.path().join("program.exe");
        let destination = directory.path().join("program.pdb");
        fs::write(&staged, b"new PDB bytes").expect("write staged PDB");
        fs::write(&outside, b"preserved").expect("write outside file");
        symlink(&outside, &destination).expect("create destination symlink");

        let error = publish_staged_pdb(Some(&staged), &output, Some("x86_64-pc-windows-msvc"))
            .expect_err("reject symlink PDB destination");

        assert_eq!(error.code(), "DebugInfoWriteFailed");
        assert_eq!(fs::read(outside).expect("read outside file"), b"preserved");
    }
}
