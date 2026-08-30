use loom_core::{Diagnostic, FileId};
use loom_syntax::{TokenKind, parse_with_file};

/// Result of conservative canonical formatting.
#[derive(Clone, Debug)]
pub struct FormatResult {
    pub text: String,
    pub diagnostics: Vec<Diagnostic>,
}

impl FormatResult {
    #[must_use]
    pub fn changed_from(&self, source: &str) -> bool {
        self.text != source
    }
}

/// Canonicalizes indentation, line endings, trailing horizontal whitespace,
/// and the final newline while preserving every non-whitespace token spelling.
/// Erroneous source is returned byte-for-byte unchanged.
#[must_use]
pub fn format_source(file: FileId, source: &str) -> FormatResult {
    let parse = parse_with_file(file, source);
    if parse.has_errors() {
        return FormatResult {
            text: source.to_owned(),
            diagnostics: parse.diagnostics().to_vec(),
        };
    }

    let mut output = String::with_capacity(source.len());
    let mut depth = 0_usize;
    let mut at_line_start = true;
    for token in parse.tokens() {
        match token.kind {
            TokenKind::Eof => break,
            TokenKind::Newline | TokenKind::Separator => {
                trim_horizontal_end(&mut output);
                output.push('\n');
                at_line_start = true;
            }
            TokenKind::Whitespace if at_line_start => {}
            TokenKind::Bom if at_line_start => output.push_str(&token.text),
            kind => {
                if at_line_start {
                    let indentation = if kind == TokenKind::RBrace {
                        depth.saturating_sub(1)
                    } else {
                        depth
                    };
                    output.push_str(&" ".repeat(indentation * 4));
                    at_line_start = false;
                }
                if kind == TokenKind::RBrace {
                    depth = depth.saturating_sub(1);
                }
                output.push_str(&token.text);
                if kind == TokenKind::LBrace {
                    depth += 1;
                }
            }
        }
    }
    trim_horizontal_end(&mut output);
    while output.ends_with('\n') {
        output.pop();
    }
    if !output.is_empty() {
        output.push('\n');
    }
    FormatResult {
        text: output,
        diagnostics: Vec::new(),
    }
}

fn trim_horizontal_end(output: &mut String) {
    while output.ends_with([' ', '\t']) {
        output.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::format_source;
    use loom_core::FileId;

    #[test]
    fn formats_while_break_and_continue_as_block_statements() {
        let source = "fn run() {\n while true {\n break\n continue\n }\n}\n";
        let formatted = format_source(FileId(0), source);
        assert!(
            formatted.diagnostics.is_empty(),
            "{:#?}",
            formatted.diagnostics
        );
        assert_eq!(
            formatted.text,
            "fn run() {\n    while true {\n        break\n        continue\n    }\n}\n"
        );
    }
}
