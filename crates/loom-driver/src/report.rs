use std::collections::BTreeMap;

use loom_core::{Diagnostic, Severity, Span};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::SourceMap;

/// Zero-based editor position. Columns are UTF-16 when used by LSP and Unicode
/// scalar counts when converted to the compiler's stable JSON span.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

/// Stable compiler JSON span from `docs/06-executable-contract.md`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SpanRecord {
    pub path: String,
    pub start_byte: u32,
    pub end_byte: u32,
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

impl SpanRecord {
    #[must_use]
    pub fn from_span(span: Span, sources: &SourceMap) -> Option<Self> {
        let source = sources.document(span.file)?;
        let start = source.scalar_position(span.range.start);
        let end = source.scalar_position(span.range.end);
        Some(Self {
            path: source.relative_path().to_owned(),
            start_byte: span.range.start,
            end_byte: span.range.end,
            start_line: start.line + 1,
            start_column: start.character + 1,
            end_line: end.line + 1,
            end_column: end.character + 1,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RelatedDiagnostic {
    pub label: String,
    pub span: SpanRecord,
}

/// Stable source-diagnostic envelope used by the command-line JSON output.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DiagnosticRecord {
    pub schema_version: u32,
    pub category: String,
    pub severity: String,
    pub code: String,
    pub message: String,
    pub primary_span: SpanRecord,
    pub related: Vec<RelatedDiagnostic>,
    pub notes: Vec<String>,
    pub details: BTreeMap<String, Value>,
}

impl DiagnosticRecord {
    #[must_use]
    pub fn from_diagnostic(diagnostic: &Diagnostic, sources: &SourceMap) -> Option<Self> {
        let primary_span = SpanRecord::from_span(diagnostic.primary, sources)?;
        let mut related = diagnostic
            .labels
            .iter()
            .filter_map(|label| {
                Some(RelatedDiagnostic {
                    label: label.message.clone(),
                    span: SpanRecord::from_span(label.span, sources)?,
                })
            })
            .collect::<Vec<_>>();
        related.sort_by(|left, right| {
            span_key(&left.span)
                .cmp(&span_key(&right.span))
                .then(left.label.cmp(&right.label))
        });
        Some(Self {
            schema_version: 1,
            category: "diagnostic".to_owned(),
            severity: severity_name(diagnostic.severity).to_owned(),
            code: diagnostic.code.clone(),
            message: diagnostic.message.clone(),
            primary_span,
            related,
            notes: diagnostic.notes.clone(),
            details: BTreeMap::new(),
        })
    }

    #[must_use]
    pub fn human(&self) -> String {
        format!(
            "{}:{}:{}: {}[{}]: {}",
            self.primary_span.path,
            self.primary_span.start_line,
            self.primary_span.start_column,
            self.severity,
            self.code,
            self.message
        )
    }
}

fn severity_name(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Information => "info",
    }
}

fn span_key(span: &SpanRecord) -> (&str, u32, u32) {
    (&span.path, span.start_byte, span.end_byte)
}
