//! Stable source identities and diagnostics shared by every compiler phase.

pub mod runtime_fault;

use std::fmt;

use serde::{Deserialize, Serialize};

/// Current source-semantics and proof-domain version.
pub const LOOM_LANGUAGE_VERSION: &str = "0.3";

/// Reserved logical package name of the compiler-distributed Loom library.
pub const STD_PACKAGE_NAME: &str = "std";

const STANDALONE_PACKAGE_NAME: &str = "<standalone>";
const STANDALONE_PACKAGE_VERSION: &str = "0";

/// Stable nominal package identity carried by every source module.
///
/// Package identity includes the resolved version so two versions of the same
/// package can coexist without merging their modules or definitions. It does
/// not authenticate source ownership: loaders of untrusted manifests,
/// registries, or artifacts must enforce their own reserved-package policy
/// before constructing compiler IR.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageId {
    name: String,
    version: String,
    language: String,
}

impl PackageId {
    #[must_use]
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self::with_language(name, version, LOOM_LANGUAGE_VERSION)
    }

    #[must_use]
    pub fn with_language(
        name: impl Into<String>,
        version: impl Into<String>,
        language: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            language: language.into(),
        }
    }

    /// Constructs the reserved nominal identity of the compiler-distributed
    /// `std` package for one language version.
    ///
    /// Its package version and source-language version deliberately advance
    /// together. Constructing this value does not make caller-supplied source
    /// compiler-owned.
    #[must_use]
    pub fn compiler_std(language: impl Into<String>) -> Self {
        let language = language.into();
        Self::with_language(STD_PACKAGE_NAME, language.clone(), language)
    }

    /// Whether this exactly matches the current compiler-owned `std` nominal
    /// identity. This comparison does not authenticate source ownership.
    #[must_use]
    pub fn is_compiler_std(&self) -> bool {
        self == &Self::compiler_std(LOOM_LANGUAGE_VERSION)
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    #[must_use]
    pub fn language(&self) -> &str {
        &self.language
    }

    /// Constructs the synthetic package identity used by source files and
    /// directories that are compiled without a manifest.
    #[must_use]
    pub fn standalone() -> Self {
        Self::with_language(
            STANDALONE_PACKAGE_NAME,
            STANDALONE_PACKAGE_VERSION,
            LOOM_LANGUAGE_VERSION,
        )
    }

    /// Whether this is the exact synthetic identity for a standalone input.
    #[must_use]
    pub fn is_standalone(&self) -> bool {
        self.name == STANDALONE_PACKAGE_NAME
            && self.version == STANDALONE_PACKAGE_VERSION
            && self.language == LOOM_LANGUAGE_VERSION
    }
}

impl Default for PackageId {
    fn default() -> Self {
        Self::standalone()
    }
}

impl fmt::Display for PackageId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}@{}", self.name, self.version)
    }
}

#[cfg(test)]
mod package_tests {
    use super::{LOOM_LANGUAGE_VERSION, PackageId, STD_PACKAGE_NAME};

    #[test]
    fn compiler_std_identity_is_exact_and_version_coupled() {
        let std = PackageId::compiler_std(LOOM_LANGUAGE_VERSION);
        assert_eq!(std.name(), STD_PACKAGE_NAME);
        assert_eq!(std.version(), LOOM_LANGUAGE_VERSION);
        assert_eq!(std.language(), LOOM_LANGUAGE_VERSION);
        assert!(std.is_compiler_std());
        assert!(!PackageId::compiler_std("0.4").is_compiler_std());
        assert!(!PackageId::with_language("std", "0.2", "0.3").is_compiler_std());
        assert!(!PackageId::with_language("stdish", "0.3", "0.3").is_compiler_std());
    }

    #[test]
    fn standalone_identity_is_the_default() {
        let standalone = PackageId::standalone();
        assert_eq!(standalone.name(), "<standalone>");
        assert_eq!(standalone.version(), "0");
        assert_eq!(standalone.language(), LOOM_LANGUAGE_VERSION);
        assert!(standalone.is_standalone());
        assert!(!PackageId::compiler_std(LOOM_LANGUAGE_VERSION).is_standalone());
        assert_eq!(PackageId::default(), standalone);
    }
}

#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct FileId(pub u32);

#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct TextRange {
    pub start: u32,
    pub end: u32,
}

impl TextRange {
    #[must_use]
    pub const fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }
}

#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct Span {
    pub file: FileId,
    pub range: TextRange,
}

impl Span {
    #[must_use]
    pub const fn new(file: FileId, start: u32, end: u32) -> Self {
        Self {
            file,
            range: TextRange::new(start, end),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
    Information,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Label {
    pub span: Span,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub code: String,
    pub severity: Severity,
    pub message: String,
    pub primary: Span,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<Label>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl Diagnostic {
    #[must_use]
    pub fn error(code: impl Into<String>, message: impl Into<String>, primary: Span) -> Self {
        Self {
            code: code.into(),
            severity: Severity::Error,
            message: message.into(),
            primary,
            labels: Vec::new(),
            notes: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_label(mut self, span: Span, message: impl Into<String>) -> Self {
        self.labels.push(Label {
            span,
            message: message.into(),
        });
        self
    }

    #[must_use]
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Name(pub String);

impl Name {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModuleName(pub String);

impl ModuleName {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ModuleName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug)]
pub struct SourceFile {
    pub id: FileId,
    pub path: String,
    pub text: String,
}
