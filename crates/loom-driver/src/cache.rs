//! Persistent content-addressed compiler cache.
//!
//! Cache identities are derived only from normalized semantic inputs. Cache
//! files are untrusted: every blob is size/hash checked and cached MIR crosses
//! the ordinary artifact decoder and MIR validator before it is returned.

use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use loom_core::FileId;
use loom_mir::{Program, decode_interpreted_artifact, encode_interpreted_artifact};
use loom_syntax::{Parse, SYNTAX_NESTING_LIMIT_VERSION};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{DiagnosticRecord, ModuleInterface, ProjectGraph, SourceMap};

const CACHE_SCHEMA_VERSION: u32 = 1;
const MAX_REF_BYTES: u64 = 64 * 1024;
const MAX_BLOB_BYTES: u64 = 1024 * 1024 * 1024;
const CHECKED_MIR_NAMESPACE: &str = "checked-mir";
const PARSE_NAMESPACE: &str = "source-parse";
const MODULE_INTERFACE_NAMESPACE: &str = "module-interface";
const TARGET_OBJECT_NAMESPACE: &str = "target-object";
const DEBUG_COMPANION_NAMESPACE: &str = "debug-companion";
const ARTIFACT_NAMESPACE: &str = "artifact";

/// All toolchain and target facts which can change checked MIR or codegen.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CacheContext {
    pub compiler_version: String,
    pub backend_version: String,
    pub standard_library_version: String,
    pub runtime_abi_version: String,
    pub target_triple: String,
    pub data_layout: String,
    pub cpu_policy: String,
    pub optimization: String,
    pub contract_mode: String,
}

/// A lowercase SHA-256 identity used by cache refs.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CacheKey(String);

impl CacheKey {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CacheKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Result of a verified cache read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CacheLookup<T> {
    Hit(T),
    Miss,
}

impl<T> CacheLookup<T> {
    #[must_use]
    pub const fn is_hit(&self) -> bool {
        matches!(self, Self::Hit(_))
    }
}

/// A validated MIR cache entry and the stable diagnostics to replay with it.
#[derive(Clone, Debug)]
pub struct CachedCompilation {
    program: Program,
    diagnostics: Vec<DiagnosticRecord>,
}

impl CachedCompilation {
    #[must_use]
    pub const fn program(&self) -> &Program {
        &self.program
    }

    #[must_use]
    pub fn into_program(self) -> Program {
        self.program
    }

    #[must_use]
    pub fn diagnostics(&self) -> &[DiagnosticRecord] {
        &self.diagnostics
    }

    #[must_use]
    pub fn into_parts(self) -> (Program, Vec<DiagnosticRecord>) {
        (self.program, self.diagnostics)
    }
}

/// A non-fatal cache write/materialization failure.
#[derive(Debug)]
pub struct CacheError {
    path: PathBuf,
    message: String,
}

impl CacheError {
    fn io(path: impl Into<PathBuf>, error: impl fmt::Display) -> Self {
        Self {
            path: path.into(),
            message: error.to_string(),
        }
    }
}

impl fmt::Display for CacheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.path.display(), self.message)
    }
}

impl std::error::Error for CacheError {}

/// Project-local content-addressed storage with verified refs and blobs.
#[derive(Clone, Debug)]
pub struct PersistentCache {
    root: PathBuf,
}

impl PersistentCache {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    #[must_use]
    pub fn for_project(project: &ProjectGraph) -> Self {
        Self::new(project.cache_root())
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Computes a relocation-independent key for an exact loaded source map.
    #[must_use]
    pub fn compilation_key(
        project: &ProjectGraph,
        sources: &SourceMap,
        context: &CacheContext,
    ) -> CacheKey {
        let mut identity = Identity::new("loom-compilation-cache-v1");
        identity.field("compiler-version", &context.compiler_version);
        identity.field("backend-version", &context.backend_version);
        identity.field("stdlib-version", &context.standard_library_version);
        identity.field("runtime-abi", &context.runtime_abi_version);
        identity.field("target-triple", &context.target_triple);
        identity.field("data-layout", &context.data_layout);
        identity.field("cpu-policy", &context.cpu_policy);
        identity.field("optimization", &context.optimization);
        identity.field("contract-mode", &context.contract_mode);
        for field in project.identity_fields() {
            identity.field("project", &field);
        }
        for source in sources.documents() {
            identity.field("source-path", source.relative_path());
            if let Some(text) = source.text() {
                identity.bytes("source-bytes", text.as_bytes());
            } else {
                identity.field("source-invalid-utf8", "true");
                identity.field("source-byte-length", &source.byte_len().to_string());
                identity.field(
                    "source-invalid-at",
                    &source.invalid_utf8_at().unwrap_or_default().to_string(),
                );
            }
        }
        identity.finish()
    }

    /// Derives a child artifact identity without including an output path.
    #[must_use]
    pub fn derived_key(parent: &CacheKey, fields: &[(&str, &str)]) -> CacheKey {
        let mut identity = Identity::new("loom-derived-cache-v1");
        identity.field("parent", parent.as_str());
        for (label, value) in fields {
            identity.field(label, value);
        }
        identity.finish()
    }

    /// Builds a standalone content identity for a compiler layer whose inputs
    /// are already expressed as semantic fingerprints.
    #[must_use]
    pub fn semantic_key(domain: &str, fields: &[(&str, &str)]) -> CacheKey {
        let mut identity = Identity::new(domain);
        for (label, value) in fields {
            identity.field(label, value);
        }
        identity.finish()
    }

    /// Computes a per-source lossless token/AST identity.
    #[must_use]
    pub fn source_parse_key(relative_path: &str, source: &str, compiler_version: &str) -> CacheKey {
        let mut identity = Identity::new("loom-source-parse-v1");
        identity.field("compiler-version", compiler_version);
        identity.field(
            "syntax-nesting-version",
            &SYNTAX_NESTING_LIMIT_VERSION.to_string(),
        );
        identity.field("source-path", relative_path);
        identity.bytes("source-bytes", source.as_bytes());
        identity.finish()
    }

    /// Loads and structurally verifies a lossless token/AST cache entry.
    #[must_use]
    pub fn load_parse(&self, key: &CacheKey, source: &str, file: FileId) -> CacheLookup<Parse> {
        let Some(bytes) = self.load_blob(PARSE_NAMESPACE, key) else {
            return CacheLookup::Miss;
        };
        let Ok(envelope) = serde_json::from_slice::<ParseEnvelope>(&bytes) else {
            return CacheLookup::Miss;
        };
        if envelope.schema_version != CACHE_SCHEMA_VERSION
            || envelope.syntax_nesting_version != SYNTAX_NESTING_LIMIT_VERSION
            || !envelope.parse.is_valid_for_source(source)
        {
            return CacheLookup::Miss;
        }
        let mut parse = envelope.parse;
        parse.rebind_file(file);
        CacheLookup::Hit(parse)
    }

    /// Stores a lossless token/AST only after checking source reconstruction.
    ///
    /// # Errors
    ///
    /// Returns a cache error when the parse/source boundary is inconsistent or
    /// atomic persistence fails.
    pub fn store_parse(
        &self,
        key: &CacheKey,
        parse: &Parse,
        source: &str,
    ) -> Result<(), CacheError> {
        if !parse.is_valid_for_source(source) {
            return Err(CacheError::io(
                &self.root,
                "parse cache entry does not reconstruct its source",
            ));
        }
        let bytes = serde_json::to_vec(&ParseEnvelope {
            schema_version: CACHE_SCHEMA_VERSION,
            syntax_nesting_version: SYNTAX_NESTING_LIMIT_VERSION,
            parse: parse.clone(),
        })
        .map_err(|error| CacheError::io(&self.root, error))?;
        self.store_blob(PARSE_NAMESPACE, key, &bytes)
    }

    /// Computes the identity of a canonical module public interface.
    #[must_use]
    pub fn module_interface_key(interface: &ModuleInterface, compiler_version: &str) -> CacheKey {
        Self::semantic_key(
            "loom-module-interface-cache-v1",
            &[
                ("compiler-version", compiler_version),
                ("module", &interface.module),
                ("interface", &interface.fingerprint),
            ],
        )
    }

    /// Confirms that an exact canonical module interface is already cached.
    #[must_use]
    pub fn load_module_interface(
        &self,
        key: &CacheKey,
        expected: &ModuleInterface,
    ) -> CacheLookup<ModuleInterface> {
        let Some(bytes) = self.load_blob(MODULE_INTERFACE_NAMESPACE, key) else {
            return CacheLookup::Miss;
        };
        let Ok(envelope) = serde_json::from_slice::<ModuleInterfaceEnvelope>(&bytes) else {
            return CacheLookup::Miss;
        };
        if envelope.schema_version != CACHE_SCHEMA_VERSION || envelope.interface != *expected {
            return CacheLookup::Miss;
        }
        CacheLookup::Hit(envelope.interface)
    }

    /// Stores a canonical module interface entry.
    ///
    /// # Errors
    ///
    /// Returns a cache error when serialization or atomic persistence fails.
    pub fn store_module_interface(
        &self,
        key: &CacheKey,
        interface: &ModuleInterface,
    ) -> Result<(), CacheError> {
        let bytes = serde_json::to_vec(&ModuleInterfaceEnvelope {
            schema_version: CACHE_SCHEMA_VERSION,
            interface: interface.clone(),
        })
        .map_err(|error| CacheError::io(&self.root, error))?;
        self.store_blob(MODULE_INTERFACE_NAMESPACE, key, &bytes)
    }

    /// Loads, decodes, and validates a cached checked-MIR entry.
    #[must_use]
    pub fn load_compilation(&self, key: &CacheKey) -> CacheLookup<CachedCompilation> {
        let Some(bytes) = self.load_blob(CHECKED_MIR_NAMESPACE, key) else {
            return CacheLookup::Miss;
        };
        let Ok(envelope) = serde_json::from_slice::<CompilationEnvelope>(&bytes) else {
            return CacheLookup::Miss;
        };
        if envelope.schema_version != CACHE_SCHEMA_VERSION
            || !valid_diagnostics(&envelope.diagnostics)
        {
            return CacheLookup::Miss;
        }
        let Ok(program) = decode_interpreted_artifact(envelope.mir.as_bytes()) else {
            return CacheLookup::Miss;
        };
        CacheLookup::Hit(CachedCompilation {
            program: program.into_program(),
            diagnostics: envelope.diagnostics,
        })
    }

    /// Stores only MIR which passes the normal artifact validation boundary.
    ///
    /// # Errors
    ///
    /// Returns a cache error if encoding or atomic persistence fails. Callers
    /// may safely continue as if caching were disabled.
    pub fn store_compilation(
        &self,
        key: &CacheKey,
        program: &Program,
        diagnostics: &[DiagnosticRecord],
    ) -> Result<(), CacheError> {
        let mir = encode_interpreted_artifact(program)
            .map_err(|error| CacheError::io(&self.root, error))?;
        let mir = String::from_utf8(mir)
            .map_err(|error| CacheError::io(&self.root, error.utf8_error()))?;
        let bytes = serde_json::to_vec(&CompilationEnvelope {
            schema_version: CACHE_SCHEMA_VERSION,
            mir,
            diagnostics: diagnostics.to_vec(),
        })
        .map_err(|error| CacheError::io(&self.root, error))?;
        self.store_blob(CHECKED_MIR_NAMESPACE, key, &bytes)
    }

    /// Loads a target object after ref, size, and content verification.
    #[must_use]
    pub fn load_target_object(&self, key: &CacheKey) -> CacheLookup<Vec<u8>> {
        self.load_blob(TARGET_OBJECT_NAMESPACE, key)
            .map_or(CacheLookup::Miss, CacheLookup::Hit)
    }

    /// Stores a relocatable target object in the content-addressed blob pool.
    ///
    /// # Errors
    ///
    /// Returns a cache error when persistence fails.
    pub fn store_target_object(&self, key: &CacheKey, bytes: &[u8]) -> Result<(), CacheError> {
        self.store_blob(TARGET_OBJECT_NAMESPACE, key, bytes)
    }

    /// Loads a platform debug companion after content verification.
    #[must_use]
    pub fn load_debug_companion(&self, key: &CacheKey) -> CacheLookup<Vec<u8>> {
        self.load_blob(DEBUG_COMPANION_NAMESPACE, key)
            .map_or(CacheLookup::Miss, CacheLookup::Hit)
    }

    /// Stores a platform debug companion payload.
    ///
    /// # Errors
    ///
    /// Returns a cache error when persistence fails.
    pub fn store_debug_companion(&self, key: &CacheKey, bytes: &[u8]) -> Result<(), CacheError> {
        self.store_blob(DEBUG_COMPANION_NAMESPACE, key, bytes)
    }

    /// Loads an arbitrary final artifact after ref and content verification.
    #[must_use]
    pub fn load_artifact(&self, key: &CacheKey) -> CacheLookup<Vec<u8>> {
        self.load_blob(ARTIFACT_NAMESPACE, key)
            .map_or(CacheLookup::Miss, CacheLookup::Hit)
    }

    /// Stores a final artifact in the content-addressed blob pool.
    ///
    /// # Errors
    ///
    /// Returns a cache error when persistence fails.
    pub fn store_artifact(&self, key: &CacheKey, bytes: &[u8]) -> Result<(), CacheError> {
        self.store_blob(ARTIFACT_NAMESPACE, key, bytes)
    }

    /// Atomically materializes verified bytes at a caller-selected output.
    ///
    /// # Errors
    ///
    /// Returns a cache error when the destination cannot be created or written.
    pub fn materialize(
        &self,
        bytes: &[u8],
        destination: &Path,
        executable: bool,
    ) -> Result<(), CacheError> {
        atomic_write(destination, bytes, executable)
    }

    fn load_blob(&self, namespace: &str, key: &CacheKey) -> Option<Vec<u8>> {
        let reference_path = self.ref_path(namespace, key);
        let reference_bytes = read_bounded(&reference_path, MAX_REF_BYTES)?;
        let reference = serde_json::from_slice::<BlobRef>(&reference_bytes).ok()?;
        if reference.schema_version != CACHE_SCHEMA_VERSION
            || reference.namespace != namespace
            || reference.key != key.as_str()
            || reference.size > MAX_BLOB_BYTES
            || !valid_digest(&reference.blob)
        {
            return None;
        }
        let blob_path = self.blob_path(&reference.blob);
        let bytes = read_bounded(&blob_path, reference.size)?;
        if u64::try_from(bytes.len()).ok()? != reference.size {
            return None;
        }
        if digest_hex(&bytes) != reference.blob {
            return None;
        }
        Some(bytes)
    }

    fn store_blob(&self, namespace: &str, key: &CacheKey, bytes: &[u8]) -> Result<(), CacheError> {
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_BLOB_BYTES {
            return Err(CacheError::io(&self.root, "cache blob exceeds 1 GiB"));
        }
        let digest = digest_hex(bytes);
        let blob_path = self.blob_path(&digest);
        let byte_length = u64::try_from(bytes.len()).expect("blob size was bounded");
        let existing_is_valid = fs::metadata(&blob_path)
            .ok()
            .filter(|metadata| metadata.len() == byte_length)
            .and_then(|_| read_bounded(&blob_path, byte_length))
            .is_some_and(|existing| digest_hex(&existing) == digest);
        if !existing_is_valid {
            atomic_write(&blob_path, bytes, false)?;
        }
        let reference = serde_json::to_vec(&BlobRef {
            schema_version: CACHE_SCHEMA_VERSION,
            namespace: namespace.to_owned(),
            key: key.as_str().to_owned(),
            blob: digest,
            size: byte_length,
        })
        .map_err(|error| CacheError::io(&self.root, error))?;
        atomic_write(&self.ref_path(namespace, key), &reference, false)
    }

    fn ref_path(&self, namespace: &str, key: &CacheKey) -> PathBuf {
        self.root
            .join("refs")
            .join(namespace)
            .join(format!("{}.json", key.as_str()))
    }

    fn blob_path(&self, digest: &str) -> PathBuf {
        self.root
            .join("blobs/sha256")
            .join(&digest[..2])
            .join(digest)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BlobRef {
    schema_version: u32,
    namespace: String,
    key: String,
    blob: String,
    size: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CompilationEnvelope {
    schema_version: u32,
    mir: String,
    diagnostics: Vec<DiagnosticRecord>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ParseEnvelope {
    schema_version: u32,
    syntax_nesting_version: u32,
    parse: Parse,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ModuleInterfaceEnvelope {
    schema_version: u32,
    interface: ModuleInterface,
}

struct Identity(Sha256);

impl Identity {
    fn new(domain: &str) -> Self {
        let mut identity = Self(Sha256::new());
        identity.field("domain", domain);
        identity
    }

    fn field(&mut self, label: &str, value: &str) {
        self.bytes(label, value.as_bytes());
    }

    fn bytes(&mut self, label: &str, value: &[u8]) {
        let label = label.as_bytes();
        self.0.update(
            u64::try_from(label.len())
                .expect("identity label length fits u64")
                .to_be_bytes(),
        );
        self.0.update(label);
        self.0.update(
            u64::try_from(value.len())
                .expect("identity value length fits u64")
                .to_be_bytes(),
        );
        self.0.update(value);
    }

    fn finish(self) -> CacheKey {
        CacheKey(format!("{:x}", self.0.finalize()))
    }
}

fn digest_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_diagnostics(diagnostics: &[DiagnosticRecord]) -> bool {
    diagnostics.iter().all(|diagnostic| {
        diagnostic.schema_version == 1
            && diagnostic.category == "diagnostic"
            && matches!(diagnostic.severity.as_str(), "error" | "warning" | "info")
    })
}

fn read_bounded(path: &Path, limit: u64) -> Option<Vec<u8>> {
    let file = fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > limit {
        return None;
    }
    let initial_capacity = usize::try_from(limit.min(1024 * 1024)).ok()?;
    let mut bytes = Vec::with_capacity(initial_capacity);
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    (u64::try_from(bytes.len()).ok()? <= limit).then_some(bytes)
}

fn atomic_write(destination: &Path, bytes: &[u8], executable: bool) -> Result<(), CacheError> {
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| CacheError::io(parent, error))?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| CacheError::io(parent, error))?;
    temporary
        .write_all(bytes)
        .map_err(|error| CacheError::io(destination, error))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| CacheError::io(destination, error))?;
    set_executable(temporary.as_file(), executable)
        .map_err(|error| CacheError::io(destination, error))?;
    temporary
        .persist(destination)
        .map_err(|error| CacheError::io(destination, error.error))?;
    Ok(())
}

#[cfg(unix)]
fn set_executable(file: &fs::File, executable: bool) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = if executable { 0o755 } else { 0o644 };
    file.set_permissions(fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_executable(_file: &fs::File, _executable: bool) -> io::Result<()> {
    Ok(())
}
