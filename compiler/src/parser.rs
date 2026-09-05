//! A newline-aware parser for the native seed's deliberately small source subset.

use crate::model::{Binary, Diagnostic, Span, Unary, ast};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Kind {
    Name(String),
    Number(String),
    LParen,
    RParen,
    LBrace,
    RBrace,
    Comma,
    Dot,
    Assign,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    Not,
    Newline,
    Eof,
}

#[derive(Clone, Debug)]
struct Token {
    kind: Kind,
    span: Span,
}

fn lex(source: usize, text: &str) -> Result<Vec<Token>, Diagnostic> {
    let mut tokens = Vec::new();
    let mut offset = 0;
    while offset < text.len() {
        let start = offset;
        let rest = &text[offset..];
        let ch = rest.chars().next().unwrap();
        offset += ch.len_utf8();
        let span = |end| Span { source, start, end };
        let kind = match ch {
            '\n' => Kind::Newline,
            '\r' => {
                if text[offset..].starts_with('\n') {
                    offset += 1;
                }
                Kind::Newline
            }
            c if c.is_whitespace() => continue,
            '/' if text[offset..].starts_with('/') => {
                while let Some(c) = text[offset..].chars().next() {
                    if c == '\n' || c == '\r' {
                        break;
                    }
                    offset += c.len_utf8();
                }
                continue;
            }
            c if c.is_ascii_digit() => {
                while text.as_bytes().get(offset).is_some_and(u8::is_ascii_digit) {
                    offset += 1;
                }
                Kind::Number(text[start..offset].into())
            }
            c if c == '_' || c.is_alphabetic() => {
                while let Some(c) = text[offset..].chars().next() {
                    if c != '_' && !c.is_alphanumeric() {
                        break;
                    }
                    offset += c.len_utf8();
                }
                Kind::Name(text[start..offset].into())
            }
            '(' => Kind::LParen,
            ')' => Kind::RParen,
            '{' => Kind::LBrace,
            '}' => Kind::RBrace,
            ',' => Kind::Comma,
            '.' => Kind::Dot,
            '+' => Kind::Plus,
            '-' => Kind::Minus,
            '*' => Kind::Star,
            '/' => Kind::Slash,
            '%' => Kind::Percent,
            '=' | '!' | '<' | '>' => {
                let paired = text[offset..].starts_with('=');
                if paired {
                    offset += 1;
                }
                match (ch, paired) {
                    ('=', true) => Kind::Eq,
                    ('=', false) => Kind::Assign,
                    ('!', true) => Kind::Ne,
                    ('!', false) => Kind::Not,
                    ('<', true) => Kind::Le,
                    ('<', false) => Kind::Lt,
                    ('>', true) => Kind::Ge,
                    ('>', false) => Kind::Gt,
                    _ => unreachable!(),
                }
            }
            '&' | '|' if text[offset..].starts_with(ch) => {
                offset += 1;
                if ch == '&' { Kind::And } else { Kind::Or }
            }
            ';' => {
                return Err(Diagnostic::new(
                    span(offset),
                    "statements use newlines, not semicolons",
                ));
            }
            ':' => {
                return Err(Diagnostic::new(
                    span(offset),
                    "write a type after its name without a colon",
                ));
            }
            _ => {
                return Err(Diagnostic::new(
                    span(offset),
                    format!("unsupported character {ch:?}"),
                ));
            }
        };
        tokens.push(Token {
            kind,
            span: span(offset),
        });
    }
    tokens.push(Token {
        kind: Kind::Eof,
        span: Span {
            source,
            start: text.len(),
            end: text.len(),
        },
    });
    Ok(tokens)
}

pub fn parse(source: usize, text: &str) -> Result<ast::File, Diagnostic> {
    Parser {
        tokens: lex(source, text)?,
        pos: 0,
    }
    .file()
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn at(&self, kind: &Kind) -> bool {
        &self.peek().kind == kind
    }

    fn word(&self, word: &str) -> bool {
        matches!(&self.peek().kind, Kind::Name(name) if name == word)
    }

    fn bump(&mut self) -> Token {
        let token = self.peek().clone();
        if token.kind != Kind::Eof {
            self.pos += 1;
        }
        token
    }

    fn eat(&mut self, kind: &Kind) -> bool {
        if self.at(kind) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn eat_word(&mut self, word: &str) -> bool {
        if self.word(word) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, kind: Kind, label: &str) -> Result<Token, Diagnostic> {
        if self.at(&kind) {
            Ok(self.bump())
        } else {
            Err(self.error(format!("expected {label}")))
        }
    }

    fn error(&self, message: impl Into<String>) -> Diagnostic {
        Diagnostic::new(self.peek().span, message)
    }

    fn newlines(&mut self) {
        while self.eat(&Kind::Newline) {}
    }

    fn name(&mut self) -> Result<(String, Span), Diagnostic> {
        match &self.peek().kind {
            Kind::Name(name) if !reserved(name) => {
                let name = name.clone();
                Ok((name, self.bump().span))
            }
            _ => Err(self.error("expected a name")),
        }
    }

    fn path(&mut self) -> Result<(Vec<String>, Span), Diagnostic> {
        let (first, mut span) = self.name()?;
        let mut path = vec![first];
        while self.eat(&Kind::Dot) {
            let (part, end) = self.name()?;
            span.end = end.end;
            path.push(part);
        }
        Ok((path, span))
    }

    fn file(mut self) -> Result<ast::File, Diagnostic> {
        let mut file = ast::File::default();
        self.newlines();
        while !self.at(&Kind::Eof) {
            if self.eat_word("import") {
                let (path, span) = self.path()?;
                if !self.at(&Kind::Newline) && !self.at(&Kind::Eof) {
                    return Err(self.error("expected a newline after import"));
                }
                file.imports.push(ast::Import { path, span });
            } else {
                file.functions.push(self.function()?);
            }
            self.newlines();
        }
        Ok(file)
    }

    fn function(&mut self) -> Result<ast::Function, Diagnostic> {
        let mut span = self.peek().span;
        let public = self.eat_word("pub");
        let test = self.eat_word("test");
        if public && test {
            return Err(self.error("test functions cannot be public"));
        }
        if !self.eat_word("fn") {
            return Err(self.error("expected a function or import declaration"));
        }
        let (name, _) = self.name()?;
        self.expect(Kind::LParen, "'('")?;
        let mut params = Vec::new();
        self.newlines();
        while !self.at(&Kind::RParen) {
            let (name, mut span) = self.name()?;
            let (ty, end) = self.name()?;
            span.end = end.end;
            params.push(ast::Param { name, ty, span });
            self.newlines();
            if !self.eat(&Kind::Comma) {
                break;
            }
            self.newlines();
        }
        self.expect(Kind::RParen, "')'")?;
        self.newlines();
        let result = if matches!(self.peek().kind, Kind::Name(_))
            && !self.word("requires")
            && !self.word("ensures")
        {
            let (name, span) = self.name()?;
            if name == "Unit" {
                return Err(Diagnostic::new(span, "omit the Unit return annotation"));
            }
            Some(name)
        } else {
            None
        };
        self.newlines();
        let mut requires = Vec::new();
        let mut ensures = Vec::new();
        while self.word("requires") || self.word("ensures") {
            let is_requires = self.eat_word("requires");
            if !is_requires {
                self.bump();
            }
            let condition = self.expr(0, false)?;
            if is_requires {
                requires.push(condition);
            } else {
                ensures.push(condition);
            }
            self.newlines();
        }
        let (body, end) = self.block()?;
        span.end = end.end;
        Ok(ast::Function {
            name,
            public,
            test,
            params,
            result,
            requires,
            ensures,
            body,
            span,
        })
    }

    fn block(&mut self) -> Result<(ast::Block, Span), Diagnostic> {
        self.newlines();
        self.expect(Kind::LBrace, "'{'")?;
        self.newlines();
        let mut statements = Vec::new();
        while !self.at(&Kind::RBrace) {
            if self.at(&Kind::Eof) {
                return Err(self.error("expected '}' to close the block"));
            }
            let statement = self.statement()?;
            // Braced control flow is self-delimiting. Other completed expressions
            // must not absorb the beginning of another same-line statement.
            let braced = matches!(
                statement.kind,
                ast::StmtKind::While { .. }
                    | ast::StmtKind::Expr(ast::Expr {
                        kind: ast::ExprKind::If { .. },
                        ..
                    })
            );
            statements.push(statement);
            if !braced && !self.at(&Kind::Newline) && !self.at(&Kind::RBrace) {
                return Err(self.error("expected a newline or '}' after the statement"));
            }
            self.newlines();
        }
        Ok((statements, self.bump().span))
    }

    fn statement(&mut self) -> Result<ast::Stmt, Diagnostic> {
        let mut span = self.peek().span;
        let kind = if self.word("let") || self.word("var") {
            let mutable = self.eat_word("var");
            if !mutable {
                self.bump();
            }
            let (name, _) = self.name()?;
            let annotation = if matches!(self.peek().kind, Kind::Name(_)) {
                Some(self.name()?.0)
            } else {
                None
            };
            self.expect(Kind::Assign, "'='")?;
            ast::StmtKind::Let {
                name,
                mutable,
                annotation,
                value: self.expr(0, false)?,
            }
        } else if self.eat_word("return") {
            let value = if self.at(&Kind::Newline) || self.at(&Kind::RBrace) || self.at(&Kind::Eof)
            {
                None
            } else {
                Some(self.expr(0, false)?)
            };
            ast::StmtKind::Return(value)
        } else if self.eat_word("assert") {
            ast::StmtKind::Assert(self.expr(0, false)?)
        } else if self.eat_word("discard") {
            ast::StmtKind::Discard(self.expr(0, false)?)
        } else if self.eat_word("while") {
            let condition = self.expr(0, false)?;
            let (body, _) = self.block()?;
            ast::StmtKind::While { condition, body }
        } else if matches!(&self.peek().kind, Kind::Name(_))
            && self
                .tokens
                .get(self.pos + 1)
                .is_some_and(|t| t.kind == Kind::Assign)
        {
            let (name, _) = self.name()?;
            self.bump();
            ast::StmtKind::Assign {
                name,
                value: self.expr(0, false)?,
            }
        } else {
            ast::StmtKind::Expr(self.expr(0, false)?)
        };
        span.end = self.tokens[self.pos.saturating_sub(1)].span.end;
        Ok(ast::Stmt { kind, span })
    }

    fn expr(&mut self, min_precedence: u8, multiline: bool) -> Result<ast::Expr, Diagnostic> {
        // A newline may follow an unfinished operator, opening delimiter, or '='.
        self.newlines();
        let mut left = self.prefix(multiline)?;
        loop {
            if multiline {
                self.newlines();
            }
            let Some((operator, precedence)) = binary(&self.peek().kind) else {
                break;
            };
            if precedence < min_precedence {
                break;
            }
            self.bump();
            let right = self.expr(precedence + 1, multiline)?;
            let span = Span {
                end: right.span.end,
                ..left.span
            };
            left = ast::Expr {
                kind: ast::ExprKind::Binary(operator, Box::new(left), Box::new(right)),
                span,
            };
        }
        Ok(left)
    }

    fn prefix(&mut self, multiline: bool) -> Result<ast::Expr, Diagnostic> {
        if self.word("if") {
            return self.if_expr();
        }
        let token = self.bump();
        let mut span = token.span;
        let kind =
            match token.kind {
                Kind::Number(number) => ast::ExprKind::Int(number.parse().map_err(|_| {
                    Diagnostic::new(span, "integer literal is outside the Int range")
                })?),
                Kind::Name(ref name) if name == "true" || name == "false" => {
                    ast::ExprKind::Bool(name == "true")
                }
                Kind::Name(ref name) if name == "Unit" => {
                    return Err(Diagnostic::new(span, "omit explicit Unit expressions"));
                }
                Kind::Name(ref name) if !reserved(name) => {
                    self.pos -= 1;
                    let (path, path_span) = self.path()?;
                    span = path_span;
                    if self.eat(&Kind::LParen) {
                        let mut args = Vec::new();
                        self.newlines();
                        while !self.at(&Kind::RParen) {
                            args.push(self.expr(0, true)?);
                            self.newlines();
                            if !self.eat(&Kind::Comma) {
                                break;
                            }
                            self.newlines();
                        }
                        span.end = self.expect(Kind::RParen, "')'")?.span.end;
                        ast::ExprKind::Call(path, args)
                    } else {
                        ast::ExprKind::Name(path)
                    }
                }
                Kind::Minus | Kind::Not => {
                    let operator = if token.kind == Kind::Minus {
                        Unary::Neg
                    } else {
                        Unary::Not
                    };
                    self.newlines();
                    // The positive magnitude of Int::MIN is not itself an Int.
                    if operator == Unary::Neg {
                        if let Kind::Number(number) = &self.peek().kind {
                            let value = number
                                .parse::<u64>()
                                .ok()
                                .filter(|n| *n <= (i64::MAX as u64) + 1)
                                .ok_or_else(|| {
                                    Diagnostic::new(
                                        self.peek().span,
                                        "integer literal is outside the Int range",
                                    )
                                })?;
                            span.end = self.bump().span.end;
                            return Ok(ast::Expr {
                                kind: ast::ExprKind::Int(if value == (i64::MAX as u64) + 1 {
                                    i64::MIN
                                } else {
                                    -(value as i64)
                                }),
                                span,
                            });
                        }
                    }
                    let value = self.expr(7, multiline)?;
                    span.end = value.span.end;
                    ast::ExprKind::Unary(operator, Box::new(value))
                }
                Kind::LParen => {
                    let value = self.expr(0, true)?;
                    self.newlines();
                    span.end = self.expect(Kind::RParen, "')'")?.span.end;
                    return Ok(ast::Expr { span, ..value });
                }
                _ => return Err(Diagnostic::new(span, "expected an expression")),
            };
        Ok(ast::Expr { kind, span })
    }

    fn if_expr(&mut self) -> Result<ast::Expr, Diagnostic> {
        let mut span = self.bump().span;
        let condition = Box::new(self.expr(0, false)?);
        let (then_body, end) = self.block()?;
        span.end = end.end;
        let before_newlines = self.pos;
        self.newlines();
        let else_body = if self.eat_word("else") {
            self.newlines();
            if self.word("if") {
                let nested = self.if_expr()?;
                span.end = nested.span.end;
                Some(vec![ast::Stmt {
                    span: nested.span,
                    kind: ast::StmtKind::Expr(nested),
                }])
            } else {
                let (body, end) = self.block()?;
                span.end = end.end;
                Some(body)
            }
        } else {
            self.pos = before_newlines;
            None
        };
        Ok(ast::Expr {
            kind: ast::ExprKind::If {
                condition,
                then_body,
                else_body,
            },
            span,
        })
    }
}

fn reserved(name: &str) -> bool {
    matches!(
        name,
        "fn" | "pub"
            | "test"
            | "import"
            | "let"
            | "var"
            | "return"
            | "assert"
            | "discard"
            | "while"
            | "if"
            | "else"
            | "requires"
            | "ensures"
            | "true"
            | "false"
    )
}

fn binary(kind: &Kind) -> Option<(Binary, u8)> {
    Some(match kind {
        Kind::Or => (Binary::Or, 1),
        Kind::And => (Binary::And, 2),
        Kind::Eq => (Binary::Eq, 3),
        Kind::Ne => (Binary::Ne, 3),
        Kind::Lt => (Binary::Lt, 4),
        Kind::Le => (Binary::Le, 4),
        Kind::Gt => (Binary::Gt, 4),
        Kind::Ge => (Binary::Ge, 4),
        Kind::Plus => (Binary::Add, 5),
        Kind::Minus => (Binary::Sub, 5),
        Kind::Star => (Binary::Mul, 6),
        Kind::Slash => (Binary::Div, 6),
        Kind::Percent => (Binary::Rem, 6),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn functions_contracts_and_test_forms() {
        let source = "import app.math.add\n\
            pub fn twice(value Int) Int\nrequires value >= 0\n\
            ensures result == value + value\n{ add(value, value) }\n\
            test fn example() { assert twice(2) == 4 }";
        let file = parse(3, source).unwrap();
        assert_eq!(file.imports[0].path, ["app", "math", "add"]);
        let function = &file.functions[0];
        assert!(function.public);
        assert_eq!(function.params[0].ty, "Int");
        assert_eq!(function.result.as_deref(), Some("Int"));
        assert_eq!(function.requires.len(), 1);
        assert_eq!(function.ensures.len(), 1);
        assert!(file.functions[1].test);
        assert_eq!(function.span.source, 3);
        assert_eq!(
            &source[function.span.start..function.span.end],
            "pub fn twice(value Int) Int\nrequires value >= 0\nensures result == value + value\n{ add(value, value) }"
        );
    }

    #[test]
    fn newlines_return_and_expression_continuations() {
        let file = parse(
            0,
            "fn stop() { return // comment\n-1\n}\n\
            fn sum() Int { let x = 1 +\n2 * 3\n(x\n+ 4) }",
        )
        .unwrap();
        assert!(matches!(
            file.functions[0].body[0].kind,
            ast::StmtKind::Return(None)
        ));
        assert!(matches!(
            file.functions[0].body[1].kind,
            ast::StmtKind::Expr(ast::Expr {
                kind: ast::ExprKind::Int(-1),
                ..
            })
        ));
        let ast::StmtKind::Let { value, .. } = &file.functions[1].body[0].kind else {
            panic!()
        };
        let ast::ExprKind::Binary(Binary::Add, _, right) = &value.kind else {
            panic!()
        };
        assert!(matches!(
            right.kind,
            ast::ExprKind::Binary(Binary::Mul, _, _)
        ));
        assert_eq!(file.functions[1].body.len(), 2);
    }

    #[test]
    fn conditionals_loops_and_minimum_integer() {
        let file = parse(
            0,
            "fn choose(x Int) Int {\nvar n Int = -9223372036854775808\n\
            while n < 0 { n = n + 1 }\n\
            if x > 1 { x }\nelse if x == 1 { 0 } else { -1 }\n}",
        )
        .unwrap();
        assert!(matches!(
            file.functions[0].body[0].kind,
            ast::StmtKind::Let {
                value: ast::Expr {
                    kind: ast::ExprKind::Int(i64::MIN),
                    ..
                },
                ..
            }
        ));
        assert!(matches!(
            file.functions[0].body[1].kind,
            ast::StmtKind::While { .. }
        ));
        let ast::StmtKind::Expr(value) = &file.functions[0].body[2].kind else {
            panic!()
        };
        let ast::ExprKind::If {
            else_body: Some(body),
            ..
        } = &value.kind
        else {
            panic!()
        };
        assert!(matches!(
            body[0].kind,
            ast::StmtKind::Expr(ast::Expr {
                kind: ast::ExprKind::If { .. },
                ..
            })
        ));
    }

    #[test]
    fn invalid_syntax_and_utf8_have_source_diagnostics() {
        for source in [
            "fn f() Unit {}",
            "fn f() { Unit }",
            "fn f(x: Int) {}",
            "fn f() { 1; }",
            "fn f() { let x = 1 let y = 2 }",
            "fn f() { await f() }",
            "fn f() Int { 9223372036854775808 }",
            "fn f() Int { -9223372036854775809 }",
            "fn f() { ( }",
            "fn f() {",
        ] {
            assert!(parse(0, source).is_err(), "unexpectedly accepted {source}");
        }
        let source = "// 🧵\nfn föö() { 🧶 }";
        let error = parse(7, source).unwrap_err();
        assert_eq!(error.span.source, 7);
        assert_eq!(&source[error.span.start..error.span.end], "🧶");
    }
}
