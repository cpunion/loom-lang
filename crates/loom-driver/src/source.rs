use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use loom_core::{FileId, PackageId};

use crate::project::ProjectGraph;
use crate::{Position, Range};

/// A project-loading failure. These are host/configuration failures, not Loom
/// source diagnostics.
#[derive(Debug)]
pub enum DriverError {
    Io {
        path: PathBuf,
        source: io::Error,
    },
    InvalidRoot(PathBuf),
    NonUtf8Path(PathBuf),
    TooManyFiles(usize),
    SourceTooLarge {
        path: PathBuf,
        bytes: usize,
    },
    PathOutsideProject {
        root: PathBuf,
        path: PathBuf,
    },
    Manifest {
        path: PathBuf,
        message: String,
    },
    UnsupportedLanguageVersion {
        path: PathBuf,
        found: String,
        supported: &'static str,
    },
    OfflineRegistryMiss {
        path: PathBuf,
        package: String,
        version: Option<String>,
    },
}

impl DriverError {
    fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedLanguageVersion { .. } => "UnsupportedLanguageVersion",
            Self::OfflineRegistryMiss { .. } => "OfflineRegistryMiss",
            _ => "ProjectLoadFailed",
        }
    }
}

impl fmt::Display for DriverError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::InvalidRoot(path) => write!(
                formatter,
                "project input must be a directory or a .loom file: {}",
                path.display()
            ),
            Self::NonUtf8Path(path) => {
                write!(
                    formatter,
                    "source path is not valid UTF-8: {}",
                    path.display()
                )
            }
            Self::TooManyFiles(count) => {
                write!(
                    formatter,
                    "project has {count} files, exceeding FileId capacity"
                )
            }
            Self::SourceTooLarge { path, bytes } => write!(
                formatter,
                "source file has {bytes} bytes, exceeding span capacity: {}",
                path.display()
            ),
            Self::PathOutsideProject { root, path } => write!(
                formatter,
                "{} is outside project root {}",
                path.display(),
                root.display()
            ),
            Self::Manifest { path, message } => {
                write!(formatter, "{}: {message}", path.display())
            }
            Self::UnsupportedLanguageVersion {
                path,
                found,
                supported,
            } => write!(
                formatter,
                "{}: language version `{found}` is incompatible with supported version `{supported}`",
                path.display()
            ),
            Self::OfflineRegistryMiss {
                path,
                package,
                version,
            } => write!(
                formatter,
                "{}: offline registry cache has no validated package `{}{}`",
                path.display(),
                package,
                version
                    .as_deref()
                    .map_or_else(String::new, |version| format!("@{version}"))
            ),
        }
    }
}

impl std::error::Error for DriverError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Provenance of one source document in a compiler snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceOrigin {
    /// A real source file selected from the root package or a source dependency.
    FileSystem,
    /// An implementation source decoded from a portable `.loomlib` artifact.
    PortableLibrary,
    /// A standard-library source embedded in and owned by the compiler.
    CompilerOwnedStandardLibrary,
}

fn is_authoritative_compiler_standard(origin: SourceOrigin, package: Option<&PackageId>) -> bool {
    matches!(origin, SourceOrigin::CompilerOwnedStandardLibrary)
        && package.is_some_and(PackageId::is_compiler_standard)
}

/// One source document in a snapshot.
#[derive(Clone, Debug)]
pub struct SourceDocument {
    id: FileId,
    absolute_path: PathBuf,
    relative_path: String,
    package: Option<PackageId>,
    is_root_package: bool,
    origin: SourceOrigin,
    text: Option<String>,
    byte_len: u32,
    line_starts: Vec<u32>,
    invalid_utf8_at: Option<u32>,
}

impl SourceDocument {
    #[must_use]
    pub const fn id(&self) -> FileId {
        self.id
    }

    #[must_use]
    pub fn absolute_path(&self) -> &Path {
        &self.absolute_path
    }

    #[must_use]
    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }

    /// Resolved package owning this source, or `None` for legacy inputs.
    #[must_use]
    pub const fn package(&self) -> Option<&PackageId> {
        self.package.as_ref()
    }

    /// Whether this source belongs to the selected root package.
    ///
    /// Use [`Self::is_read_only`] for source-mutation policy: compiler-owned
    /// and portable-library sources remain immutable regardless of package
    /// ownership.
    #[must_use]
    pub const fn is_root_package(&self) -> bool {
        self.is_root_package
    }

    /// The source's explicit compiler provenance.
    #[must_use]
    pub const fn origin(&self) -> SourceOrigin {
        self.origin
    }

    /// Whether this document was decoded from a portable `.loomlib` dependency.
    #[must_use]
    pub const fn is_embedded_dependency(&self) -> bool {
        matches!(self.origin, SourceOrigin::PortableLibrary)
    }

    /// Whether this document is part of the compiler-owned standard library.
    #[must_use]
    pub const fn is_compiler_owned(&self) -> bool {
        matches!(self.origin, SourceOrigin::CompilerOwnedStandardLibrary)
    }

    /// Whether both source ownership and nominal package identity identify the
    /// compiler-distributed standard library.
    #[must_use]
    pub(crate) fn is_authoritative_compiler_standard(&self) -> bool {
        is_authoritative_compiler_standard(self.origin, self.package.as_ref())
    }

    /// Whether an editor can navigate to a real backing source file.
    #[must_use]
    pub const fn is_navigable(&self) -> bool {
        matches!(self.origin, SourceOrigin::FileSystem)
    }

    /// Whether source-mutating tools must reject edits to this document.
    #[must_use]
    pub const fn is_read_only(&self) -> bool {
        !self.is_root_package || !self.is_navigable()
    }

    /// Returns source text, or `None` when the file contains invalid UTF-8.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }

    #[must_use]
    pub const fn byte_len(&self) -> u32 {
        self.byte_len
    }

    #[must_use]
    pub const fn invalid_utf8_at(&self) -> Option<u32> {
        self.invalid_utf8_at
    }

    /// Converts a UTF-8 byte offset to a zero-based scalar position.
    #[must_use]
    pub fn scalar_position(&self, byte: u32) -> Position {
        self.position_impl(byte, ColumnEncoding::Scalar)
    }

    /// Converts a UTF-8 byte offset to a zero-based UTF-16 LSP position.
    #[must_use]
    pub fn utf16_position(&self, byte: u32) -> Position {
        self.position_impl(byte, ColumnEncoding::Utf16)
    }

    /// Converts a zero-based UTF-16 LSP position to a UTF-8 byte offset.
    #[must_use]
    pub fn byte_offset_utf16(&self, position: Position) -> Option<u32> {
        let text = self.text()?;
        let line_index = usize::try_from(position.line).ok()?;
        let line_start = *self.line_starts.get(line_index)?;
        let line_end = self
            .line_starts
            .get(line_index.checked_add(1)?)
            .copied()
            .unwrap_or(self.byte_len);
        let line_start_usize = usize::try_from(line_start).ok()?;
        let line_end_usize = usize::try_from(line_end).ok()?;
        let mut content_end = line_end_usize;
        if text.as_bytes().get(content_end.saturating_sub(1)) == Some(&b'\n') {
            content_end -= 1;
            if text.as_bytes().get(content_end.saturating_sub(1)) == Some(&b'\r') {
                content_end -= 1;
            }
        }
        let line = &text[line_start_usize..content_end];
        let mut utf16 = 0_u32;
        for (offset, character) in line.char_indices() {
            if utf16 == position.character {
                return Some(line_start + u32::try_from(offset).ok()?);
            }
            utf16 += u32::try_from(character.len_utf16()).ok()?;
            if utf16 > position.character {
                return None;
            }
        }
        if utf16 == position.character {
            u32::try_from(content_end).ok()
        } else {
            None
        }
    }

    #[must_use]
    pub fn utf16_range(&self, start: u32, end: u32) -> Range {
        Range {
            start: self.utf16_position(start),
            end: self.utf16_position(end),
        }
    }

    fn position_impl(&self, byte: u32, encoding: ColumnEncoding) -> Position {
        let bounded = byte.min(self.byte_len);
        let line = self
            .line_starts
            .partition_point(|start| *start <= bounded)
            .saturating_sub(1);
        let line_start = self.line_starts[line];
        let column = self.text.as_ref().map_or(bounded - line_start, |text| {
            let safe_end = floor_char_boundary(
                text,
                usize::try_from(bounded).expect("u32 byte offset fits usize"),
            );
            let prefix =
                &text[usize::try_from(line_start).expect("u32 byte offset fits usize")..safe_end];
            match encoding {
                ColumnEncoding::Scalar => {
                    u32::try_from(prefix.chars().count()).expect("source length fits u32")
                }
                ColumnEncoding::Utf16 => {
                    u32::try_from(prefix.encode_utf16().count()).expect("source length fits u32")
                }
            }
        });
        Position {
            line: u32::try_from(line).expect("source length bounds line count"),
            character: column,
        }
    }
}

#[derive(Clone, Copy)]
enum ColumnEncoding {
    Scalar,
    Utf16,
}

/// Stable `FileId` assignment plus bidirectional source/path lookup.
#[derive(Clone, Debug)]
pub struct SourceMap {
    root: PathBuf,
    documents: Vec<SourceDocument>,
    by_path: BTreeMap<PathBuf, FileId>,
}

impl SourceMap {
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn documents(&self) -> &[SourceDocument] {
        &self.documents
    }

    #[must_use]
    pub fn document(&self, id: FileId) -> Option<&SourceDocument> {
        self.documents.get(usize::try_from(id.0).ok()?)
    }

    #[must_use]
    pub fn file_id(&self, path: &Path) -> Option<FileId> {
        let normalized = if path.is_absolute() {
            normalize_absolute(path)
        } else {
            normalize_absolute(&self.root.join(path))
        };
        self.by_path.get(&normalized).copied()
    }

    pub(crate) fn load(
        project: &ProjectGraph,
        overlays: &BTreeMap<PathBuf, String>,
    ) -> Result<Self, DriverError> {
        let root = project.root().to_path_buf();
        let mut paths = project
            .source_files()?
            .into_iter()
            .map(|source| {
                (
                    source.absolute,
                    (
                        source.stable_path,
                        source.package,
                        source.is_root_package,
                        source.embedded_text,
                        source.origin,
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>();
        for overlay in overlays.keys() {
            let overlay = normalize_absolute(overlay);
            if overlay.strip_prefix(&root).is_err() {
                return Err(DriverError::PathOutsideProject {
                    root: root.clone(),
                    path: overlay,
                });
            }
            if let Some(stable_path) = project.overlay_stable_path(&overlay) {
                let package = project.root_package().map(|package| package.id().clone());
                paths.insert(
                    overlay,
                    (stable_path, package, true, None, SourceOrigin::FileSystem),
                );
            }
        }
        let mut paths = paths.into_iter().collect::<Vec<_>>();
        paths.sort_by(|(_, left), (_, right)| left.0.cmp(&right.0));

        if u32::try_from(paths.len()).is_err() {
            return Err(DriverError::TooManyFiles(paths.len()));
        }

        let mut documents = Vec::with_capacity(paths.len());
        let mut by_path = BTreeMap::new();
        for (index, (path, (relative_path, package, is_root_package, embedded_text, origin))) in
            paths.into_iter().enumerate()
        {
            let id = FileId(u32::try_from(index).expect("file count was checked"));
            // Compiler-owned standard sources and portable-library payloads
            // are immutable inputs. Their synthetic paths live below the
            // project root for stable diagnostics, but an editor overlay at
            // that path must never replace trusted embedded bytes.
            let bytes = if let Some(text) = embedded_text {
                text.into_bytes()
            } else if let Some(text) = overlays.get(&path) {
                text.as_bytes().to_vec()
            } else {
                fs::read(&path).map_err(|error| DriverError::io(path.clone(), error))?
            };
            let byte_len = u32::try_from(bytes.len()).map_err(|_| DriverError::SourceTooLarge {
                path: path.clone(),
                bytes: bytes.len(),
            })?;
            let (text, invalid_utf8_at, line_starts) = match String::from_utf8(bytes) {
                Ok(text) => {
                    let starts = compute_line_starts(&text);
                    (Some(text), None, starts)
                }
                Err(error) => {
                    let valid = error.utf8_error().valid_up_to();
                    let prefix = std::str::from_utf8(&error.as_bytes()[..valid])
                        .expect("valid_up_to prefix must be valid UTF-8");
                    (
                        None,
                        Some(u32::try_from(valid).unwrap_or(u32::MAX)),
                        compute_line_starts(prefix),
                    )
                }
            };
            let path = normalize_absolute(&path);
            by_path.insert(path.clone(), id);
            documents.push(SourceDocument {
                id,
                absolute_path: path,
                relative_path,
                package,
                is_root_package,
                origin,
                text,
                byte_len,
                line_starts,
                invalid_utf8_at,
            });
        }
        Ok(Self {
            root,
            documents,
            by_path,
        })
    }
}

/// Recursively discovers `.loom` files in deterministic project-relative order.
/// Directory symlinks are not followed; every `.git` and `target` subtree is
/// ignored regardless of nesting depth.
///
/// # Errors
///
/// Returns [`DriverError`] when the root is not a readable directory or a
/// discovered project-relative path is not valid UTF-8.
pub fn discover_loom_files(root: &Path) -> Result<Vec<PathBuf>, DriverError> {
    let canonical = fs::canonicalize(root).map_err(|error| DriverError::io(root, error))?;
    if !canonical.is_dir() {
        return Err(DriverError::InvalidRoot(canonical));
    }
    let mut pending = vec![canonical.clone()];
    let mut files = BTreeSet::new();
    while let Some(directory) = pending.pop() {
        let entries =
            fs::read_dir(&directory).map_err(|error| DriverError::io(directory.clone(), error))?;
        let mut entries = entries
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| DriverError::io(directory.clone(), error))?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries.into_iter().rev() {
            let file_type = entry
                .file_type()
                .map_err(|error| DriverError::io(entry.path(), error))?;
            let path = entry.path();
            if file_type.is_dir() {
                if !is_ignored_name(&entry.file_name()) {
                    pending.push(path);
                }
            } else if file_type.is_file() && has_loom_extension(&path) {
                files.insert(normalize_absolute(&path));
            }
        }
    }
    let mut keyed = files
        .into_iter()
        .map(|path| {
            relative_key(&canonical, &path)
                .map(|key| (key, path.clone()))
                .ok_or(DriverError::NonUtf8Path(path))
        })
        .collect::<Result<Vec<_>, _>>()?;
    keyed.sort_by(|(left, _), (right, _)| left.cmp(right));
    Ok(keyed.into_iter().map(|(_, path)| path).collect())
}

pub(crate) fn normalized_project_path(root: &Path, path: &Path) -> Result<PathBuf, DriverError> {
    let absolute = if path.is_absolute() {
        normalize_absolute(path)
    } else {
        normalize_absolute(&root.join(path))
    };
    if absolute.strip_prefix(root).is_err() {
        return Err(DriverError::PathOutsideProject {
            root: root.to_path_buf(),
            path: absolute,
        });
    }
    Ok(absolute)
}

pub(crate) fn is_ignored_relative(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root).is_ok_and(|relative| {
        relative.components().any(|component| {
            let Component::Normal(name) = component else {
                return false;
            };
            is_ignored_name(name)
        })
    })
}

fn is_ignored_name(name: &std::ffi::OsStr) -> bool {
    name == ".git" || name == "target"
}

pub(crate) fn has_loom_extension(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "loom")
}

pub(crate) fn relative_key(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(value) => parts.push(value.to_str()?.to_owned()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(parts.join("/"))
}

pub(crate) fn normalize_absolute(path: &Path) -> PathBuf {
    let lexical = normalize_lexical(path);
    if let Ok(canonical) = fs::canonicalize(&lexical) {
        return canonical;
    }

    let mut existing = lexical.as_path();
    let mut suffix = Vec::new();
    while let Some(parent) = existing.parent() {
        if let Some(name) = existing.file_name() {
            suffix.push(name.to_owned());
        }
        if let Ok(mut canonical) = fs::canonicalize(parent) {
            for component in suffix.iter().rev() {
                canonical.push(component);
            }
            return normalize_lexical(&canonical);
        }
        existing = parent;
    }
    lexical
}

fn normalize_lexical(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn compute_line_starts(text: &str) -> Vec<u32> {
    let mut starts = vec![0];
    for (offset, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(u32::try_from(offset + 1).unwrap_or(u32::MAX));
        }
    }
    starts
}

fn floor_char_boundary(text: &str, mut offset: usize) -> usize {
    offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

#[cfg(test)]
mod provenance_tests {
    use loom_core::{LOOM_LANGUAGE_VERSION, PackageId};

    use super::{SourceOrigin, is_authoritative_compiler_standard};

    #[test]
    fn compiler_standard_authority_requires_origin_and_exact_package_identity() {
        let standard = PackageId::compiler_standard(LOOM_LANGUAGE_VERSION);
        let application = PackageId::new("application", "1.0.0");

        for (origin, package, expected) in [
            (SourceOrigin::CompilerOwnedStandardLibrary, &standard, true),
            (
                SourceOrigin::CompilerOwnedStandardLibrary,
                &application,
                false,
            ),
            (SourceOrigin::FileSystem, &standard, false),
            (SourceOrigin::FileSystem, &application, false),
        ] {
            assert_eq!(
                is_authoritative_compiler_standard(origin, Some(package)),
                expected,
                "origin={origin:?}, package={package}"
            );
        }

        assert!(!is_authoritative_compiler_standard(
            SourceOrigin::PortableLibrary,
            Some(&standard),
        ));
        assert!(!is_authoritative_compiler_standard(
            SourceOrigin::CompilerOwnedStandardLibrary,
            None,
        ));
    }
}
