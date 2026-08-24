#![no_main]

use libfuzzer_sys::fuzz_target;
use loom_core::FileId;
use loom_syntax::{TokenKind, lex, parse_with_file};

fuzz_target!(|bytes: &[u8]| {
    let Ok(source) = std::str::from_utf8(bytes) else {
        return;
    };
    let lexed = lex(source);
    assert_eq!(lexed.reconstructed(), source);
    assert!(matches!(
        lexed.tokens.last().map(|token| token.kind),
        Some(TokenKind::Eof)
    ));
    let source_len = u32::try_from(source.len()).unwrap_or(u32::MAX);
    for token in &lexed.tokens {
        assert!(token.range.start <= token.range.end);
        assert!(token.range.end <= source_len);
    }
    for tokens in lexed.tokens.windows(2) {
        assert!(tokens[0].range.end <= tokens[1].range.start);
    }

    let parse = parse_with_file(FileId(7), source);
    assert_eq!(parse.source(), source);
    assert_eq!(parse.reconstructed(), source);
    for diagnostic in parse.diagnostics() {
        assert_eq!(diagnostic.primary.file, FileId(7));
        assert!(diagnostic.primary.range.start <= diagnostic.primary.range.end);
        assert!(diagnostic.primary.range.end <= source_len);
    }
});
