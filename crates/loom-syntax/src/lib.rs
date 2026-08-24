//! Lossless lexical and syntactic front end for loom-lang.

pub mod ast;
pub mod lexer;
pub mod parser;

pub use ast::*;
pub use lexer::{LexError, Lexed, Token, TokenKind, lex};
pub use parser::{MAX_SYNTAX_NESTING, Parse, SYNTAX_NESTING_LIMIT_VERSION, parse, parse_with_file};
