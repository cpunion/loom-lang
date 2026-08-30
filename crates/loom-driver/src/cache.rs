//! Persistent content-addressed compiler cache.
//!
//! Cache identities are derived only from normalized semantic inputs. Cache
//! files are untrusted for parsing and corruption: every blob is size/hash
//! checked and cached MIR crosses the ordinary artifact decoder and MIR
//! validator before it is returned. Content hashes are not authentication
//! against a principal that can rewrite both a reference and its blob.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use loom_core::{FileId, Severity};
use loom_mir::{CheckedProgram, decode_interpreted_artifact, encode_interpreted_artifact};
use loom_sema::{
    Analysis, CanonicalConcepts, CanonicalStdItems, ConstructionCheck, DefMapBuild, ImplIndex,
    ModuleGraph, RuntimeCheck, TypedProgram,
};
use loom_syntax::{Parse, SYNTAX_NESTING_LIMIT_VERSION};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::incremental::ModuleQueryKey;
use crate::{DiagnosticRecord, ModuleInterface, ProjectGraph, SourceMap};

pub const CACHE_SCHEMA_VERSION: u32 = 14;
const MAX_REF_BYTES: u64 = 64 * 1024;
const MAX_BLOB_BYTES: u64 = 1024 * 1024 * 1024;
const CHECKED_MIR_NAMESPACE: &str = "checked-mir";
const PARSE_NAMESPACE: &str = "source-parse";
const MODULE_INTERFACE_NAMESPACE: &str = "module-interface";
const TYPED_MODULE_STATE_NAMESPACE: &str = "typed-module-state";
const TARGET_OBJECT_NAMESPACE: &str = "target-object";
const ARTIFACT_NAMESPACE: &str = "artifact";
const COMPILATION_CACHE_DOMAIN: &str = "loom-compilation-cache-v14";

/// Frontend facts which can change validated checked MIR.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CacheContext {
    pub language_version: String,
    pub frontend_identity: String,
    pub stdlib_identity: String,
    pub contract_mode: String,
}

#[cfg(test)]
mod tests {
    use loom_sema::{CallResolution, CallTarget, Substitution, TaskIntrinsic};

    #[test]
    fn compiler_std_call_resolution_round_trips_stable_identity() {
        for item in [
            TaskIntrinsic::Sleep,
            TaskIntrinsic::All,
            TaskIntrinsic::Settled,
            TaskIntrinsic::Any,
            TaskIntrinsic::Race,
        ] {
            let resolution = CallResolution {
                target: CallTarget::TaskIntrinsic(item),
                substitution: Substitution::default(),
                dispatch_witness: None,
                witnesses: Vec::new(),
                receiver: None,
            };

            let bytes = serde_json::to_vec(&resolution).expect("serialize call resolution");
            let decoded: CallResolution =
                serde_json::from_slice(&bytes).expect("deserialize call resolution");

            assert_eq!(decoded, resolution);
            assert_eq!(decoded.target, CallTarget::TaskIntrinsic(item));
        }
    }
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
    program: CheckedProgram,
    diagnostics: Vec<DiagnosticRecord>,
}

pub(crate) struct CachedSemanticState {
    pub keys: BTreeMap<String, ModuleQueryKey>,
    pub analysis: Analysis,
}

impl CachedCompilation {
    #[must_use]
    pub const fn program(&self) -> &CheckedProgram {
        &self.program
    }

    #[must_use]
    pub fn diagnostics(&self) -> &[DiagnosticRecord] {
        &self.diagnostics
    }

    #[must_use]
    pub fn into_parts(self) -> (CheckedProgram, Vec<DiagnosticRecord>) {
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

/// Deterministic cache inventory used by `loom cache stat`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct CacheStats {
    pub schema_version: u32,
    pub references: u64,
    pub invalid_references: u64,
    pub blobs: u64,
    pub bytes: u64,
    pub reclaimable_blobs: u64,
    pub reclaimable_bytes: u64,
}

/// Files removed by an explicit cache prune operation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct CachePruneReport {
    pub invalid_references_removed: u64,
    pub blobs_removed: u64,
    pub bytes_reclaimed: u64,
}

#[derive(Default)]
struct CacheInventory {
    stats: CacheStats,
    invalid_references: Vec<PathBuf>,
    orphan_blobs: Vec<(PathBuf, u64)>,
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

    /// Inventories valid refs and content blobs without trusting cache files.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when an existing cache directory cannot be read.
    pub fn stats(&self) -> Result<CacheStats, CacheError> {
        self.inventory().map(|inventory| inventory.stats)
    }

    /// Removes malformed refs and blobs which are not reachable from a valid
    /// ref. The operation is confined to this exact versioned cache root.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when inventory or removal fails.
    pub fn prune(&self) -> Result<CachePruneReport, CacheError> {
        let inventory = self.inventory()?;
        for path in &inventory.invalid_references {
            fs::remove_file(path).map_err(|error| CacheError::io(path, error))?;
        }
        for (path, _) in &inventory.orphan_blobs {
            fs::remove_file(path).map_err(|error| CacheError::io(path, error))?;
        }
        Ok(CachePruneReport {
            invalid_references_removed: u64::try_from(inventory.invalid_references.len())
                .unwrap_or(u64::MAX),
            blobs_removed: u64::try_from(inventory.orphan_blobs.len()).unwrap_or(u64::MAX),
            bytes_reclaimed: inventory.orphan_blobs.iter().map(|(_, bytes)| *bytes).sum(),
        })
    }

    fn inventory(&self) -> Result<CacheInventory, CacheError> {
        let reference_paths = regular_files(&self.root.join("refs"))?;
        let blob_paths = regular_files(&self.root.join("blobs/sha256"))?;
        let mut reachable = BTreeSet::new();
        let mut inventory = CacheInventory::default();
        inventory.stats.schema_version = CACHE_SCHEMA_VERSION;
        inventory.stats.references = u64::try_from(reference_paths.len()).unwrap_or(u64::MAX);
        for path in reference_paths {
            let valid = fs::read(&path)
                .ok()
                .filter(|bytes| u64::try_from(bytes.len()).unwrap_or(u64::MAX) <= MAX_REF_BYTES)
                .and_then(|bytes| serde_json::from_slice::<BlobRef>(&bytes).ok())
                .filter(|reference| {
                    reference.schema_version == CACHE_SCHEMA_VERSION
                        && valid_digest(&reference.blob)
                        && reference.size <= MAX_BLOB_BYTES
                });
            if let Some(reference) = valid {
                reachable.insert(reference.blob);
            } else {
                inventory.stats.invalid_references += 1;
                inventory.invalid_references.push(path);
            }
        }
        inventory.stats.blobs = u64::try_from(blob_paths.len()).unwrap_or(u64::MAX);
        for path in blob_paths {
            let bytes = fs::metadata(&path)
                .map_err(|error| CacheError::io(&path, error))?
                .len();
            inventory.stats.bytes = inventory.stats.bytes.saturating_add(bytes);
            let digest = path.file_name().and_then(|name| name.to_str());
            if digest.is_none_or(|digest| !valid_digest(digest) || !reachable.contains(digest)) {
                inventory.stats.reclaimable_blobs += 1;
                inventory.stats.reclaimable_bytes =
                    inventory.stats.reclaimable_bytes.saturating_add(bytes);
                inventory.orphan_blobs.push((path, bytes));
            }
        }
        Ok(inventory)
    }

    /// Computes a relocation-independent key for an exact loaded source map.
    #[must_use]
    pub fn compilation_key(
        project: &ProjectGraph,
        sources: &SourceMap,
        context: &CacheContext,
    ) -> CacheKey {
        let mut identity = Identity::new(COMPILATION_CACHE_DOMAIN);
        identity.field("language-version", &context.language_version);
        identity.field("frontend-identity", &context.frontend_identity);
        identity.field("stdlib-identity", &context.stdlib_identity);
        identity.field("contract-mode", &context.contract_mode);
        for field in project.semantic_identity_fields() {
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
        let mut identity = Identity::new("loom-derived-cache-v3");
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
        let mut identity = Identity::new("loom-source-parse-v3");
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
            "loom-module-interface-cache-v3",
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

    /// Builds the stable slot used to carry reusable typed body facts for a
    /// declaration-compatible module graph. Body fingerprints deliberately do
    /// not enter this key: the payload records them per module so one changed
    /// body can reuse every unchanged module across compiler processes.
    #[must_use]
    pub(crate) fn typed_module_state_key(
        keys: &BTreeMap<String, ModuleQueryKey>,
        compiler_version: &str,
    ) -> CacheKey {
        let mut identity = Identity::new("loom-typed-module-state-v2");
        identity.field("compiler-version", compiler_version);
        for (module, key) in keys {
            identity.field("module", module);
            identity.field("interface", &key.interface_fingerprint);
            identity.field("shape", &key.shape_fingerprint);
        }
        identity.finish()
    }

    /// Loads typed semantic facts only for the exact declaration-compatible
    /// graph. Cache bytes remain untrusted; reuse is additionally guarded by a
    /// panic boundary in the driver and falls back to a fresh analysis.
    #[must_use]
    pub(crate) fn load_typed_module_state(
        &self,
        key: &CacheKey,
        expected: &BTreeMap<String, ModuleQueryKey>,
    ) -> CacheLookup<CachedSemanticState> {
        let Some(bytes) = self.load_blob(TYPED_MODULE_STATE_NAMESPACE, key) else {
            return CacheLookup::Miss;
        };
        let Ok(envelope) = serde_json::from_slice::<TypedModuleStateEnvelope>(&bytes) else {
            return CacheLookup::Miss;
        };
        if envelope.schema_version != CACHE_SCHEMA_VERSION
            || !same_module_shapes(&envelope.keys, expected)
            || typed_program_contains_process_local_proofs(&envelope.analysis.typed)
            || envelope
                .analysis
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.severity == Severity::Error)
        {
            return CacheLookup::Miss;
        }
        CacheLookup::Hit(CachedSemanticState {
            keys: envelope.keys,
            analysis: envelope.analysis.into_analysis(),
        })
    }

    /// Atomically stores per-module body fingerprints with their typed facts.
    ///
    /// # Errors
    ///
    /// Returns an error on serialization or persistence failure. Compiler
    /// callers may continue as if semantic caching were disabled.
    pub(crate) fn store_typed_module_state(
        &self,
        key: &CacheKey,
        keys: &BTreeMap<String, ModuleQueryKey>,
        analysis: &Analysis,
    ) -> Result<(), CacheError> {
        if analysis
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
        {
            return Err(CacheError::io(
                &self.root,
                "typed module state cannot contain error diagnostics",
            ));
        }
        if typed_program_contains_process_local_proofs(&analysis.typed) {
            // A digest checks bytes against their reference; it does not prove
            // the compiler conclusion inside those bytes. Proof-bearing bodies
            // are deliberately rebuilt from source instead of publishing a
            // semantic entry which a later process must reject.
            return Ok(());
        }
        let bytes = serde_json::to_vec(&TypedModuleStateEnvelope {
            schema_version: CACHE_SCHEMA_VERSION,
            keys: keys.clone(),
            analysis: SemanticAnalysisWire::from_analysis(analysis),
        })
        .map_err(|error| CacheError::io(&self.root, error))?;
        self.store_blob(TYPED_MODULE_STATE_NAMESPACE, key, &bytes)
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
        if program.serialized_construction_proofs_were_distrusted() {
            return CacheLookup::Miss;
        }
        CacheLookup::Hit(CachedCompilation {
            program,
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
        program: &CheckedProgram,
        diagnostics: &[DiagnosticRecord],
    ) -> Result<(), CacheError> {
        if program.requires_serialized_construction_replay() {
            // The ordinary artifact codec correctly turns `Proven` into
            // `Recheck`, but a compiler-cache hit must retain the same route
            // and check elimination as a fresh source build. Rebuild this
            // compilation from the exact keyed sources instead.
            return Ok(());
        }
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
            atomic_write_idempotent(&blob_path, bytes)?;
        }
        let reference = serde_json::to_vec(&BlobRef {
            schema_version: CACHE_SCHEMA_VERSION,
            namespace: namespace.to_owned(),
            key: key.as_str().to_owned(),
            blob: digest,
            size: byte_length,
        })
        .map_err(|error| CacheError::io(&self.root, error))?;
        atomic_write_idempotent(&self.ref_path(namespace, key), &reference)
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

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TypedModuleStateEnvelope {
    schema_version: u32,
    keys: BTreeMap<String, ModuleQueryKey>,
    analysis: SemanticAnalysisWire,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SemanticAnalysisWire {
    typed: TypedProgram,
    module_graph: ModuleGraph,
    def_maps: DefMapBuild,
    impl_index: ImplIndex,
    diagnostics: Vec<loom_core::Diagnostic>,
}

impl SemanticAnalysisWire {
    fn from_analysis(analysis: &Analysis) -> Self {
        Self {
            typed: analysis.typed.clone(),
            module_graph: analysis.module_graph.clone(),
            def_maps: analysis.def_maps.clone(),
            impl_index: analysis.impl_index.clone(),
            diagnostics: analysis.diagnostics.clone(),
        }
    }

    fn into_analysis(self) -> Analysis {
        Analysis {
            typed: self.typed,
            module_graph: self.module_graph,
            def_maps: self.def_maps,
            impl_index: self.impl_index,
            // Persistent typed facts are reuse inputs, not language-item
            // proof authority. `analyze_reusing_bodies` always resolves this
            // identity again from the current module-qualified HIR before MIR
            // lowering can observe the returned Analysis.
            canonical_concepts: CanonicalConcepts::default(),
            canonical_std_items: CanonicalStdItems::default(),
            diagnostics: self.diagnostics,
        }
    }
}

fn same_module_shapes(
    cached: &BTreeMap<String, ModuleQueryKey>,
    expected: &BTreeMap<String, ModuleQueryKey>,
) -> bool {
    cached.len() == expected.len()
        && expected.iter().all(|(module, current)| {
            cached.get(module).is_some_and(|previous| {
                previous.module == current.module
                    && previous.interface_fingerprint == current.interface_fingerprint
                    && previous.shape_fingerprint == current.shape_fingerprint
            })
        })
}

fn typed_program_contains_process_local_proofs(program: &TypedProgram) -> bool {
    program.bodies.iter().any(|(_, body)| {
        body.construction_checks
            .values()
            .any(|check| *check == ConstructionCheck::Proven)
            || body
                .assertion_checks
                .values()
                .any(|check| *check == RuntimeCheck::Proven)
            || body.contract_check == Some(RuntimeCheck::Proven)
    })
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

fn regular_files(root: &Path) -> Result<Vec<PathBuf>, CacheError> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory)
            .map_err(|error| CacheError::io(&directory, error))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| CacheError::io(&directory, error))?;
        for entry in entries {
            let kind = entry
                .file_type()
                .map_err(|error| CacheError::io(entry.path(), error))?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                files.push(entry.path());
            }
        }
    }
    files.sort();
    Ok(files)
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
    #[cfg(unix)]
    set_executable(temporary.as_file(), executable)
        .map_err(|error| CacheError::io(destination, error))?;
    #[cfg(not(unix))]
    let _ = executable;
    temporary
        .persist(destination)
        .map_err(|error| CacheError::io(destination, error.error))?;
    Ok(())
}

fn atomic_write_idempotent(destination: &Path, bytes: &[u8]) -> Result<(), CacheError> {
    if file_contents_match(destination, bytes) {
        return Ok(());
    }

    let result = atomic_write(destination, bytes, false);
    if result.is_err() && file_contents_match(destination, bytes) {
        // Another writer may have published the same immutable cache entry
        // while this writer was renaming its temporary file. Windows can
        // reject that competing replacement even though the desired bytes are
        // already complete at the destination.
        return Ok(());
    }
    result
}

fn file_contents_match(path: &Path, expected: &[u8]) -> bool {
    let Ok(length) = u64::try_from(expected.len()) else {
        return false;
    };
    fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.file_type().is_file() && metadata.len() == length)
        && read_bounded(path, length).is_some_and(|bytes| bytes == expected)
}

#[cfg(unix)]
fn set_executable(file: &fs::File, executable: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = if executable { 0o755 } else { 0o644 };
    file.set_permissions(fs::Permissions::from_mode(mode))
}
