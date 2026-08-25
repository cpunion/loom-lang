use std::collections::BTreeMap;
use std::fmt::Write as _;

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

    /// Renders a compact compiler diagnostic with a source line, underline,
    /// related locations, and notes. JSON output remains the stable machine
    /// contract; this rendering is intentionally human-oriented.
    #[must_use]
    pub fn human_with_source(&self, sources: &SourceMap) -> String {
        let mut output = self.human();
        if let Some(source) = sources
            .documents()
            .iter()
            .find(|source| source.relative_path() == self.primary_span.path)
            .filter(|source| !source.is_embedded_dependency())
            && let Some(text) = source.text()
            && let Some(line) = text.lines().nth(
                usize::try_from(self.primary_span.start_line.saturating_sub(1))
                    .unwrap_or(usize::MAX),
            )
        {
            let line_number = self.primary_span.start_line;
            let gutter_width = line_number.to_string().len();
            let _ = write!(output, "\n{line_number:>gutter_width$} | {line}\n");
            let line_width = u32::try_from(line.chars().count()).unwrap_or(u32::MAX);
            let start = self
                .primary_span
                .start_column
                .saturating_sub(1)
                .min(line_width);
            let end = if self.primary_span.end_line == self.primary_span.start_line {
                self.primary_span.end_column.saturating_sub(1)
            } else {
                line_width
            }
            .min(line_width);
            let width = end.saturating_sub(start).max(1).min(line_width.max(1));
            let _ = write!(
                output,
                "{space:>gutter_width$} | {indent}{carets}",
                space = "",
                indent = " ".repeat(usize::try_from(start).unwrap_or(0)),
                carets = "^".repeat(usize::try_from(width).unwrap_or(1)),
            );
        }
        for related in &self.related {
            let _ = write!(
                output,
                "\n  = {}:{}:{}: {}",
                related.span.path,
                related.span.start_line,
                related.span.start_column,
                related.label
            );
        }
        for note in &self.notes {
            let _ = write!(output, "\n  = note: {note}");
        }
        output
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
