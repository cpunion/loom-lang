//! UTF-8, lossless lexer for Loom source files.
//!
//! The lexer keeps whitespace, comments, byte-order marks, and both kinds of
//! newline token.  [`TokenKind::Separator`] is a syntactically significant
//! newline; [`TokenKind::Newline`] is a physical newline suppressed by the
//! continuation rules. Concatenating every token's `text` therefore always
//! reconstructs the input exactly, including erroneous input.

use loom_core::TextRange;
use serde::{Deserialize, Serialize};
use unicode_ident::{is_xid_continue, is_xid_start};

/// A lexical token. Tokens own their spelling so a lexed file is self-contained.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Token {
    pub kind: TokenKind,
    pub range: TextRange,
    pub text: String,
}

impl Token {
    #[must_use]
    pub fn is_trivia(&self) -> bool {
        self.kind.is_trivia()
    }
}

/// All tokens recognized by the Core 0.1 and Core 0.2 surface grammar.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum TokenKind {
    Bom,
    Whitespace,
    Newline,
    Separator,
    LineComment,
    DocComment,

    Ident,
    IntLiteral,
    FloatLiteral,
    TextLiteral,

    ModuleKw,
    ImportKw,
    PubKw,
    TypeKw,
    WhereKw,
    RecordKw,
    InvariantKw,
    EnumKw,
    FnKw,
    ImplKw,
    MethodKw,
    MutKw,
    SelfValueKw,
    LetKw,
    VarKw,
    ScopedKw,
    DeferKw,
    DiscardKw,
    AsyncKw,
    AwaitKw,
    IfKw,
    ElseKw,
    MatchKw,
    ReturnKw,
    AssertKw,
    RequiresKw,
    EnsuresKw,
    TestKw,
    TrueKw,
    FalseKw,
    OldKw,
    ResultKw,

    ConceptKw,
    DynKw,
    AssociatedKw,
    StaticKw,
    AsKw,
    ForKw,
    InKw,
    ViewKw,
    BoxKw,
    SharedKw,

    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Dot,
    DotDot,
    Colon,
    Eq,
    FatArrow,
    Plus,
    Minus,
    Star,
    Slash,
    EqEq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    AndAnd,
    OrOr,
    Bang,
    Question,
    Underscore,

    Unknown,
    Eof,
}

impl TokenKind {
    #[must_use]
    pub const fn is_trivia(self) -> bool {
        matches!(
            self,
            Self::Bom | Self::Whitespace | Self::Newline | Self::LineComment | Self::DocComment
        )
    }

    #[must_use]
    pub const fn is_keyword(self) -> bool {
        matches!(
            self,
            Self::ModuleKw
                | Self::ImportKw
                | Self::PubKw
                | Self::TypeKw
                | Self::WhereKw
                | Self::RecordKw
                | Self::InvariantKw
                | Self::EnumKw
                | Self::FnKw
                | Self::ImplKw
                | Self::MethodKw
                | Self::MutKw
                | Self::SelfValueKw
                | Self::LetKw
                | Self::VarKw
                | Self::ScopedKw
                | Self::DeferKw
                | Self::DiscardKw
                | Self::AsyncKw
                | Self::AwaitKw
                | Self::IfKw
                | Self::ElseKw
                | Self::MatchKw
                | Self::ReturnKw
                | Self::AssertKw
                | Self::RequiresKw
                | Self::EnsuresKw
                | Self::TestKw
                | Self::TrueKw
                | Self::FalseKw
                | Self::OldKw
                | Self::ResultKw
                | Self::ConceptKw
                | Self::DynKw
                | Self::AssociatedKw
                | Self::StaticKw
                | Self::AsKw
                | Self::ForKw
                | Self::InKw
                | Self::ViewKw
                | Self::BoxKw
                | Self::SharedKw
        )
    }

    const fn continues_line(self) -> bool {
        matches!(
            self,
            Self::LParen
                | Self::LBracket
                | Self::LBrace
                | Self::Comma
                | Self::Dot
                | Self::DotDot
                | Self::Colon
                | Self::Eq
                | Self::FatArrow
                | Self::Plus
                | Self::Minus
                | Self::Star
                | Self::Slash
                | Self::EqEq
                | Self::NotEq
                | Self::Lt
                | Self::LtEq
                | Self::Gt
                | Self::GtEq
                | Self::AndAnd
                | Self::OrOr
                | Self::Bang
        )
    }
}

/// A source-local lexical error. [`crate::parse_with_file`] turns these into
/// compiler diagnostics by attaching a file id.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LexError {
    pub code: &'static str,
    pub message: String,
    pub range: TextRange,
}

/// Complete lossless result of lexing a source string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Lexed {
    pub tokens: Vec<Token>,
    pub errors: Vec<LexError>,
}

impl Lexed {
    #[must_use]
    pub fn reconstructed(&self) -> String {
        self.tokens
            .iter()
            .map(|token| token.text.as_str())
            .collect()
    }

    #[must_use]
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }
}

/// Lexes `source`, preserving every byte in exactly one token.
#[must_use]
pub fn lex(source: &str) -> Lexed {
    Lexer::new(source).run()
}

struct Lexer<'a> {
    source: &'a str,
    offset: usize,
    tokens: Vec<Token>,
    errors: Vec<LexError>,
    paren_depth: u32,
    bracket_depth: u32,
    previous_significant: Option<TokenKind>,
}

impl<'a> Lexer<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            offset: 0,
            tokens: Vec::new(),
            errors: Vec::new(),
            paren_depth: 0,
            bracket_depth: 0,
            previous_significant: None,
        }
    }

    fn run(mut self) -> Lexed {
        while self.offset < self.source.len() {
            let start = self.offset;
            let ch = self.peek_char().expect("offset is before end");
            match ch {
                '\u{feff}' => {
                    self.bump_char();
                    let kind = if start == 0 {
                        TokenKind::Bom
                    } else {
                        self.error(
                            "InvalidSourceCharacter",
                            "a byte-order mark is only allowed at the start of a file",
                            start,
                            self.offset,
                        );
                        TokenKind::Unknown
                    };
                    self.push(kind, start);
                }
                ' ' | '\t' => self.scan_whitespace(start),
                '\r' | '\n' => self.scan_newline(start),
                '/' if self.rest().starts_with("///") => self.scan_comment(start, true),
                '/' if self.rest().starts_with("//") => self.scan_comment(start, false),
                '"' => self.scan_string(start),
                '0'..='9' => self.scan_number(start),
                '_' => self.scan_underscore_or_ident(start),
                _ if is_xid_start(ch) => self.scan_ident(start),
                _ => self.scan_punctuation_or_unknown(start, ch),
            }
        }

        let end = to_u32(self.source.len());
        self.tokens.push(Token {
            kind: TokenKind::Eof,
            range: TextRange::new(end, end),
            text: String::new(),
        });
        Lexed {
            tokens: self.tokens,
            errors: self.errors,
        }
    }

    fn scan_whitespace(&mut self, start: usize) {
        self.take_while(|ch| matches!(ch, ' ' | '\t'));
        self.push(TokenKind::Whitespace, start);
    }

    fn scan_newline(&mut self, start: usize) {
        if self.rest().starts_with("\r\n") {
            self.offset += 2;
        } else {
            self.bump_char();
        }
        let suppressed = self.paren_depth > 0
            || self.bracket_depth > 0
            || self
                .previous_significant
                .is_some_and(TokenKind::continues_line);
        self.push(
            if suppressed {
                TokenKind::Newline
            } else {
                TokenKind::Separator
            },
            start,
        );
    }

    fn scan_comment(&mut self, start: usize, doc: bool) {
        self.take_while(|ch| !matches!(ch, '\r' | '\n'));
        self.push(
            if doc {
                TokenKind::DocComment
            } else {
                TokenKind::LineComment
            },
            start,
        );
    }

    fn scan_string(&mut self, start: usize) {
        self.bump_char();
        let mut terminated = false;
        while let Some(ch) = self.peek_char() {
            match ch {
                '"' => {
                    self.bump_char();
                    terminated = true;
                    break;
                }
                '\r' | '\n' => break,
                '\\' => self.scan_escape(start),
                _ => {
                    self.bump_char();
                }
            }
        }
        if !terminated {
            let (code, message) = if self.peek_char().is_some_and(|ch| matches!(ch, '\r' | '\n')) {
                (
                    "NewlineInString",
                    "text literal must close before the end of the line",
                )
            } else {
                (
                    "UnterminatedString",
                    "text literal must close before the end of the file",
                )
            };
            self.error(code, message, start, self.offset);
        }
        self.push(TokenKind::TextLiteral, start);
    }

    fn scan_escape(&mut self, string_start: usize) {
        let escape_start = self.offset;
        self.bump_char();
        let Some(ch) = self.peek_char() else {
            self.error(
                "InvalidEscape",
                "incomplete escape sequence",
                escape_start,
                self.offset,
            );
            return;
        };
        match ch {
            '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't' | '0' => {
                self.bump_char();
            }
            'u' => {
                self.bump_char();
                self.scan_unicode_escape(escape_start);
            }
            '\r' | '\n' => {
                self.error(
                    "InvalidEscape",
                    "a backslash cannot continue a text literal across a line",
                    escape_start,
                    self.offset,
                );
            }
            _ => {
                self.bump_char();
                self.error(
                    "InvalidEscape",
                    "unknown text escape; use JSON escapes, \\0, or \\u{...}",
                    escape_start,
                    self.offset,
                );
            }
        }
        debug_assert!(self.offset > string_start);
    }

    fn scan_unicode_escape(&mut self, escape_start: usize) {
        if self.peek_char() == Some('{') {
            self.bump_char();
            let digits_start = self.offset;
            self.take_while(|ch| ch.is_ascii_hexdigit());
            let digits = &self.source[digits_start..self.offset];
            let closed = self.peek_char() == Some('}');
            if closed {
                self.bump_char();
            }
            let valid_scalar = (1..=6).contains(&digits.len())
                && u32::from_str_radix(digits, 16)
                    .ok()
                    .and_then(char::from_u32)
                    .is_some();
            if !closed || !valid_scalar {
                self.error(
                    "InvalidUnicodeEscape",
                    "braced Unicode escape must contain 1 to 6 hex digits naming a scalar value",
                    escape_start,
                    self.offset,
                );
            }
            return;
        }

        let digits_start = self.offset;
        for _ in 0..4 {
            if self.peek_char().is_some_and(|ch| ch.is_ascii_hexdigit()) {
                self.bump_char();
            } else {
                break;
            }
        }
        let digits = &self.source[digits_start..self.offset];
        if digits.len() != 4 {
            self.error(
                "InvalidUnicodeEscape",
                "JSON Unicode escape must contain exactly four hex digits naming a scalar value",
                escape_start,
                self.offset,
            );
            return;
        }

        let unit = u16::from_str_radix(digits, 16).expect("four hex digits fit in u16");
        if (0xd800..=0xdbff).contains(&unit) {
            let pair_start = self.offset;
            if self.rest().starts_with("\\u") {
                self.offset += 2;
                let low_start = self.offset;
                for _ in 0..4 {
                    if self.peek_char().is_some_and(|ch| ch.is_ascii_hexdigit()) {
                        self.bump_char();
                    } else {
                        break;
                    }
                }
                let low_digits = &self.source[low_start..self.offset];
                let valid_low = low_digits.len() == 4
                    && u16::from_str_radix(low_digits, 16)
                        .is_ok_and(|low| (0xdc00..=0xdfff).contains(&low));
                if valid_low {
                    return;
                }
            }
            self.error(
                "InvalidUnicodeEscape",
                "a high-surrogate JSON escape must be followed by a low-surrogate escape",
                escape_start,
                self.offset.max(pair_start),
            );
        } else if (0xdc00..=0xdfff).contains(&unit) {
            self.error(
                "InvalidUnicodeEscape",
                "a low-surrogate JSON escape must follow a high-surrogate escape",
                escape_start,
                self.offset,
            );
        }
    }

    fn scan_number(&mut self, start: usize) {
        self.take_while(|ch| ch.is_ascii_digit());
        let mut float = false;

        if self.peek_char() == Some('.')
            && self
                .peek_second_char()
                .is_some_and(|ch| ch.is_ascii_digit())
        {
            float = true;
            self.bump_char();
            self.take_while(|ch| ch.is_ascii_digit());
        }

        if matches!(self.peek_char(), Some('e' | 'E')) {
            float = true;
            self.bump_char();
            if matches!(self.peek_char(), Some('+' | '-')) {
                self.bump_char();
            }
            let exponent_start = self.offset;
            self.take_while(|ch| ch.is_ascii_digit());
            if self.offset == exponent_start {
                self.error(
                    "InvalidFloatLiteral",
                    "a Float exponent requires at least one decimal digit",
                    start,
                    self.offset,
                );
            }
        }

        if self.peek_char() == Some('_') {
            self.take_while(|ch| ch == '_' || ch.is_ascii_alphanumeric());
            self.error(
                if float {
                    "InvalidFloatLiteral"
                } else {
                    "InvalidIntegerLiteral"
                },
                "numeric separators are not part of the Loom decimal literal grammar",
                start,
                self.offset,
            );
        }

        self.push(
            if float {
                TokenKind::FloatLiteral
            } else {
                TokenKind::IntLiteral
            },
            start,
        );
    }

    fn scan_underscore_or_ident(&mut self, start: usize) {
        self.bump_char();
        if self.peek_char().is_some_and(is_xid_continue) {
            self.take_while(is_xid_continue);
            self.push(TokenKind::Ident, start);
        } else {
            self.push(TokenKind::Underscore, start);
        }
    }

    fn scan_ident(&mut self, start: usize) {
        self.bump_char();
        self.take_while(is_xid_continue);
        let kind = keyword(&self.source[start..self.offset]);
        self.push(kind, start);
    }

    fn scan_punctuation_or_unknown(&mut self, start: usize, ch: char) {
        let (kind, bytes) = if self.rest().starts_with("..") {
            (TokenKind::DotDot, 2)
        } else if self.rest().starts_with("=>") {
            (TokenKind::FatArrow, 2)
        } else if self.rest().starts_with("==") {
            (TokenKind::EqEq, 2)
        } else if self.rest().starts_with("!=") {
            (TokenKind::NotEq, 2)
        } else if self.rest().starts_with("<=") {
            (TokenKind::LtEq, 2)
        } else if self.rest().starts_with(">=") {
            (TokenKind::GtEq, 2)
        } else if self.rest().starts_with("&&") {
            (TokenKind::AndAnd, 2)
        } else if self.rest().starts_with("||") {
            (TokenKind::OrOr, 2)
        } else {
            let kind = match ch {
                '(' => TokenKind::LParen,
                ')' => TokenKind::RParen,
                '[' => TokenKind::LBracket,
                ']' => TokenKind::RBracket,
                '{' => TokenKind::LBrace,
                '}' => TokenKind::RBrace,
                ',' => TokenKind::Comma,
                '.' => TokenKind::Dot,
                ':' => TokenKind::Colon,
                '=' => TokenKind::Eq,
                '+' => TokenKind::Plus,
                '-' => TokenKind::Minus,
                '*' => TokenKind::Star,
                '/' => TokenKind::Slash,
                '<' => TokenKind::Lt,
                '>' => TokenKind::Gt,
                '!' => TokenKind::Bang,
                '?' => TokenKind::Question,
                _ => TokenKind::Unknown,
            };
            (kind, ch.len_utf8())
        };
        self.offset += bytes;
        if kind == TokenKind::Unknown {
            self.error(
                "InvalidSourceCharacter",
                format!("unexpected character `{ch}`"),
                start,
                self.offset,
            );
        }
        match kind {
            TokenKind::LParen => self.paren_depth += 1,
            TokenKind::RParen => self.paren_depth = self.paren_depth.saturating_sub(1),
            TokenKind::LBracket => self.bracket_depth += 1,
            TokenKind::RBracket => self.bracket_depth = self.bracket_depth.saturating_sub(1),
            _ => {}
        }
        self.push(kind, start);
    }

    fn push(&mut self, kind: TokenKind, start: usize) {
        let token = Token {
            kind,
            range: TextRange::new(to_u32(start), to_u32(self.offset)),
            text: self.source[start..self.offset].to_owned(),
        };
        if !kind.is_trivia() && !matches!(kind, TokenKind::Separator | TokenKind::Eof) {
            self.previous_significant = Some(kind);
        }
        self.tokens.push(token);
    }

    fn error(&mut self, code: &'static str, message: impl Into<String>, start: usize, end: usize) {
        self.errors.push(LexError {
            code,
            message: message.into(),
            range: TextRange::new(to_u32(start), to_u32(end)),
        });
    }

    fn rest(&self) -> &'a str {
        &self.source[self.offset..]
    }

    fn peek_char(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn peek_second_char(&self) -> Option<char> {
        self.rest().chars().nth(1)
    }

    fn bump_char(&mut self) -> Option<char> {
        let ch = self.peek_char()?;
        self.offset += ch.len_utf8();
        Some(ch)
    }

    fn take_while(&mut self, mut predicate: impl FnMut(char) -> bool) {
        while let Some(ch) = self.peek_char() {
            if !predicate(ch) {
                break;
            }
            self.bump_char();
        }
    }
}

fn keyword(text: &str) -> TokenKind {
    match text {
        "module" => TokenKind::ModuleKw,
        "import" => TokenKind::ImportKw,
        "pub" => TokenKind::PubKw,
        "type" => TokenKind::TypeKw,
        "where" => TokenKind::WhereKw,
        "record" => TokenKind::RecordKw,
        "invariant" => TokenKind::InvariantKw,
        "enum" => TokenKind::EnumKw,
        "fn" => TokenKind::FnKw,
        "impl" => TokenKind::ImplKw,
        "method" => TokenKind::MethodKw,
        "mut" => TokenKind::MutKw,
        "self" => TokenKind::SelfValueKw,
        "let" => TokenKind::LetKw,
        "var" => TokenKind::VarKw,
        "scoped" => TokenKind::ScopedKw,
        "defer" => TokenKind::DeferKw,
        "discard" => TokenKind::DiscardKw,
        "async" => TokenKind::AsyncKw,
        "await" => TokenKind::AwaitKw,
        "if" => TokenKind::IfKw,
        "else" => TokenKind::ElseKw,
        "match" => TokenKind::MatchKw,
        "return" => TokenKind::ReturnKw,
        "assert" => TokenKind::AssertKw,
        "requires" => TokenKind::RequiresKw,
        "ensures" => TokenKind::EnsuresKw,
        "test" => TokenKind::TestKw,
        "true" => TokenKind::TrueKw,
        "false" => TokenKind::FalseKw,
        "old" => TokenKind::OldKw,
        "result" => TokenKind::ResultKw,
        "concept" => TokenKind::ConceptKw,
        "dyn" => TokenKind::DynKw,
        "associated" => TokenKind::AssociatedKw,
        "static" => TokenKind::StaticKw,
        "as" => TokenKind::AsKw,
        "for" => TokenKind::ForKw,
        "in" => TokenKind::InKw,
        "view" => TokenKind::ViewKw,
        "box" => TokenKind::BoxKw,
        "shared" => TokenKind::SharedKw,
        _ => TokenKind::Ident,
    }
}

fn to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<TokenKind> {
        lex(source)
            .tokens
            .into_iter()
            .filter(|token| !token.kind.is_trivia())
            .map(|token| token.kind)
            .collect()
    }

    #[test]
    fn every_input_byte_is_preserved() {
        let source = "\u{feff}module 商店.价格\r\n/// docs\nfn f() { \"x\\n\\u{1f642}\" }\n";
        let lexed = lex(source);
        assert_eq!(lexed.reconstructed(), source);
        assert!(lexed.errors.is_empty(), "{:?}", lexed.errors);
    }

    #[test]
    fn unicode_xid_and_contextual_self_are_identifiers() {
        assert_eq!(
            kinds("Δelta Self _name _"),
            vec![
                TokenKind::Ident,
                TokenKind::Ident,
                TokenKind::Ident,
                TokenKind::Underscore,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn discard_is_a_reserved_keyword() {
        assert_eq!(
            kinds("discard discarded"),
            vec![TokenKind::DiscardKw, TokenKind::Ident, TokenKind::Eof]
        );
        assert!(TokenKind::DiscardKw.is_keyword());
    }

    #[test]
    fn newline_rules_distinguish_separator_and_continuation() {
        let lexed = lex("a\nb +\n c\nf(\n x,\n y\n)\n{\n z\n}\n");
        let newlines: Vec<_> = lexed
            .tokens
            .iter()
            .filter(|token| matches!(token.kind, TokenKind::Newline | TokenKind::Separator))
            .map(|token| token.kind)
            .collect();
        assert_eq!(
            newlines,
            vec![
                TokenKind::Separator,
                TokenKind::Newline,
                TokenKind::Separator,
                TokenKind::Newline,
                TokenKind::Newline,
                TokenKind::Newline,
                TokenKind::Separator,
                TokenKind::Newline,
                TokenKind::Separator,
                TokenKind::Separator,
            ]
        );
    }

    #[test]
    fn decimal_literal_boundary_is_deliberate() {
        assert_eq!(
            kinds("0 12 1.25 1e3 2.0E-4 .5 1."),
            vec![
                TokenKind::IntLiteral,
                TokenKind::IntLiteral,
                TokenKind::FloatLiteral,
                TokenKind::FloatLiteral,
                TokenKind::FloatLiteral,
                TokenKind::Dot,
                TokenKind::IntLiteral,
                TokenKind::IntLiteral,
                TokenKind::Dot,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn invalid_exponent_is_one_local_error() {
        let lexed = lex("12e+ next");
        assert_eq!(lexed.errors.len(), 1);
        assert_eq!(lexed.errors[0].code, "InvalidFloatLiteral");
        assert_eq!(lexed.tokens[0].kind, TokenKind::FloatLiteral);
        assert_eq!(lexed.tokens[0].text, "12e+");
    }

    #[test]
    fn numeric_separator_reports_the_literal_kind() {
        assert_eq!(lex("1_000").errors[0].code, "InvalidIntegerLiteral");
        assert_eq!(lex("1.0_0").errors[0].code, "InvalidFloatLiteral");
    }

    #[test]
    fn strings_accept_frozen_escape_set() {
        let lexed = lex(r#""\"\\\/\b\f\n\r\t\0\u0041\uD83D\uDE42\u{1f642}""#);
        assert!(lexed.errors.is_empty(), "{:?}", lexed.errors);
        assert_eq!(lexed.tokens[0].kind, TokenKind::TextLiteral);
    }

    #[test]
    fn strings_reject_unpaired_json_surrogates() {
        let high = lex(r#""\uD83D!""#);
        assert_eq!(high.errors[0].code, "InvalidUnicodeEscape");
        let low = lex(r#""\uDE42""#);
        assert_eq!(low.errors[0].code, "InvalidUnicodeEscape");
    }

    #[test]
    fn unterminated_string_stops_at_line() {
        let lexed = lex("\"broken\nmodule good\n");
        assert_eq!(lexed.errors.len(), 1);
        assert_eq!(lexed.errors[0].code, "NewlineInString");
        assert_eq!(lexed.tokens[0].text, "\"broken");
        assert!(
            lexed
                .tokens
                .iter()
                .any(|token| token.kind == TokenKind::ModuleKw)
        );
    }

    #[test]
    fn unterminated_string_at_eof_has_a_distinct_code() {
        let lexed = lex("\"broken");
        assert_eq!(lexed.errors.len(), 1);
        assert_eq!(lexed.errors[0].code, "UnterminatedString");
    }

    #[test]
    fn bom_is_only_trivia_at_byte_zero() {
        let lexed = lex("\u{feff}a\u{feff}b");
        assert_eq!(lexed.tokens[0].kind, TokenKind::Bom);
        assert_eq!(lexed.tokens[2].kind, TokenKind::Unknown);
        assert_eq!(lexed.errors[0].code, "InvalidSourceCharacter");
    }
}
