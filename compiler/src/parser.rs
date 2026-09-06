//! A newline-aware parser for the native seed's deliberately small source subset.

use crate::model::{Binary, Diagnostic, Span, Unary, ast};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Kind {
    Name(String),
    Number(String),
    Text(String),
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Dot,
    Assign,
    Arrow,
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
            '"' => Kind::Text(string_literal(text, &mut offset, span(start + 1))?),
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
            '[' => Kind::LBracket,
            ']' => Kind::RBracket,
            ',' => Kind::Comma,
            '.' => Kind::Dot,
            '+' => Kind::Plus,
            '-' => Kind::Minus,
            '*' => Kind::Star,
            '/' => Kind::Slash,
            '%' => Kind::Percent,
            '=' if text[offset..].starts_with('>') => {
                offset += 1;
                Kind::Arrow
            }
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

fn string_literal(text: &str, offset: &mut usize, mut span: Span) -> Result<String, Diagnostic> {
    let mut value = String::new();
    while let Some(ch) = text[*offset..].chars().next() {
        *offset += ch.len_utf8();
        match ch {
            '"' => return Ok(value),
            '\n' | '\r' => {
                span.end = *offset;
                return Err(Diagnostic::new(
                    span,
                    "escape newlines inside string literals",
                ));
            }
            '\\' => {
                let escaped = text[*offset..].chars().next();
                if let Some(ch) = escaped {
                    *offset += ch.len_utf8();
                }
                let ch = match escaped {
                    Some('n') => '\n',
                    Some('r') => '\r',
                    Some('t') => '\t',
                    Some('0') => '\0',
                    Some('"') => '"',
                    Some('\\') => '\\',
                    Some('u') if text[*offset..].starts_with('{') => {
                        *offset += 1;
                        let start = *offset;
                        while text
                            .as_bytes()
                            .get(*offset)
                            .is_some_and(u8::is_ascii_hexdigit)
                        {
                            *offset += 1;
                        }
                        let digits = &text[start..*offset];
                        let decoded = u32::from_str_radix(digits, 16)
                            .ok()
                            .and_then(char::from_u32);
                        if digits.len() > 6
                            || !text[*offset..].starts_with('}')
                            || decoded.is_none()
                        {
                            span.end = *offset;
                            return Err(Diagnostic::new(span, "invalid Unicode escape"));
                        }
                        *offset += 1;
                        decoded.unwrap()
                    }
                    _ => {
                        span.end = *offset;
                        return Err(Diagnostic::new(span, "invalid string escape"));
                    }
                };
                value.push(ch);
            }
            _ => value.push(ch),
        }
    }
    span.end = *offset;
    Err(Diagnostic::new(span, "unterminated string literal"))
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

    fn type_ref(&mut self) -> Result<ast::TypeRef, Diagnostic> {
        let (path, mut span) = self.path()?;
        let args = self.type_args()?;
        if !args.is_empty() {
            span.end = self.tokens[self.pos - 1].span.end;
        }
        Ok(ast::TypeRef { path, args, span })
    }

    fn type_args(&mut self) -> Result<Vec<ast::TypeRef>, Diagnostic> {
        let mut args = Vec::new();
        if self.eat(&Kind::LBracket) {
            self.newlines();
            loop {
                args.push(self.type_ref()?);
                self.newlines();
                if !self.eat(&Kind::Comma) {
                    break;
                }
                self.newlines();
                if self.at(&Kind::RBracket) {
                    break;
                }
            }
            self.expect(Kind::RBracket, "']'")?;
        }
        Ok(args)
    }

    fn parameters(&mut self) -> Result<Vec<String>, Diagnostic> {
        let mut parameters = Vec::new();
        if self.eat(&Kind::LBracket) {
            self.newlines();
            loop {
                parameters.push(self.name()?.0);
                self.newlines();
                if !self.eat(&Kind::Comma) {
                    break;
                }
                self.newlines();
                if self.at(&Kind::RBracket) {
                    break;
                }
            }
            self.expect(Kind::RBracket, "']'")?;
        }
        Ok(parameters)
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
                let span = self.peek().span;
                let public = self.eat_word("pub");
                if self.word("record") || self.word("enum") {
                    file.data.push(self.data(public, span)?);
                } else {
                    file.functions.push(self.function(public, span)?);
                }
            }
            self.newlines();
        }
        Ok(file)
    }

    fn data(&mut self, public: bool, mut span: Span) -> Result<ast::Data, Diagnostic> {
        let record = self.eat_word("record");
        if !record {
            self.bump();
        }
        let (name, _) = self.name()?;
        let parameters = self.parameters()?;
        self.newlines();
        self.expect(Kind::LBrace, "'{'")?;
        self.newlines();
        let mut fields = Vec::new();
        let mut variants = Vec::new();
        while !self.at(&Kind::RBrace) {
            let (name, mut span) = self.name()?;
            if record {
                let ty = self.type_ref()?;
                span.end = ty.span.end;
                fields.push(ast::Field { name, ty, span });
            } else {
                let mut fields = Vec::new();
                if self.eat(&Kind::LParen) {
                    self.newlines();
                    while !self.at(&Kind::RParen) {
                        fields.push(self.type_ref()?);
                        self.newlines();
                        if !self.eat(&Kind::Comma) {
                            break;
                        }
                        self.newlines();
                    }
                    span.end = self.expect(Kind::RParen, "')'")?.span.end;
                }
                variants.push(ast::Variant { name, fields, span });
            }
            self.eat(&Kind::Comma);
            self.newlines();
        }
        span.end = self.bump().span.end;
        Ok(ast::Data {
            name,
            public,
            parameters,
            kind: if record {
                ast::DataKind::Record(fields)
            } else {
                ast::DataKind::Enum(variants)
            },
            span,
        })
    }

    fn function(&mut self, public: bool, mut span: Span) -> Result<ast::Function, Diagnostic> {
        let test = self.eat_word("test");
        let intrinsic = self.eat_word("intrinsic");
        if public && test {
            return Err(self.error("test functions cannot be public"));
        }
        if intrinsic && (public || test) {
            return Err(self.error("intrinsic declarations must be private and cannot be tests"));
        }
        if !self.eat_word("fn") {
            return Err(self.error("expected a function, record, enum, or import declaration"));
        }
        let (name, _) = self.name()?;
        let parameters = self.parameters()?;
        self.expect(Kind::LParen, "'('")?;
        let mut params = Vec::new();
        self.newlines();
        while !self.at(&Kind::RParen) {
            let (name, mut span) = self.name()?;
            let ty = self.type_ref()?;
            span.end = ty.span.end;
            params.push(ast::Param { name, ty, span });
            self.newlines();
            if !self.eat(&Kind::Comma) {
                break;
            }
            self.newlines();
        }
        self.expect(Kind::RParen, "')'")?;
        if !intrinsic {
            self.newlines();
        }
        let result = if matches!(self.peek().kind, Kind::Name(_))
            && !self.word("requires")
            && !self.word("ensures")
        {
            let ty = self.type_ref()?;
            if ty.path == ["Unit"] {
                return Err(Diagnostic::new(ty.span, "omit the Unit return annotation"));
            }
            Some(ty)
        } else {
            None
        };
        if intrinsic {
            if !self.at(&Kind::Newline) && !self.at(&Kind::Eof) {
                return Err(self.error("intrinsic declarations have no body or contracts"));
            }
            span.end = self.tokens[self.pos - 1].span.end;
            return Ok(ast::Function {
                name,
                public,
                test,
                intrinsic,
                parameters,
                params,
                result,
                requires: Vec::new(),
                ensures: Vec::new(),
                body: Vec::new(),
                span,
            });
        }
        self.newlines();
        let mut requires = Vec::new();
        let mut ensures = Vec::new();
        while self.word("requires") || self.word("ensures") {
            let is_requires = self.eat_word("requires");
            if !is_requires {
                self.bump();
            }
            let condition = self.expr(0, false, false)?;
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
            intrinsic,
            parameters,
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
                        kind: ast::ExprKind::If { .. }
                            | ast::ExprKind::Match { .. }
                            | ast::ExprKind::Block(_),
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
                Some(self.type_ref()?)
            } else {
                None
            };
            self.expect(Kind::Assign, "'='")?;
            ast::StmtKind::Let {
                name,
                mutable,
                annotation,
                value: self.expr(0, false, true)?,
            }
        } else if self.eat_word("return") {
            let value = if self.at(&Kind::Newline) || self.at(&Kind::RBrace) || self.at(&Kind::Eof)
            {
                None
            } else {
                Some(self.expr(0, false, true)?)
            };
            ast::StmtKind::Return(value)
        } else if self.eat_word("assert") {
            ast::StmtKind::Assert(self.expr(0, false, true)?)
        } else if self.eat_word("discard") {
            ast::StmtKind::Discard(self.expr(0, false, true)?)
        } else if self.eat_word("while") {
            let condition = self.expr(0, false, false)?;
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
                value: self.expr(0, false, true)?,
            }
        } else {
            ast::StmtKind::Expr(self.expr(0, false, true)?)
        };
        span.end = self.tokens[self.pos.saturating_sub(1)].span.end;
        Ok(ast::Stmt { kind, span })
    }

    fn expr(
        &mut self,
        min_precedence: u8,
        multiline: bool,
        records: bool,
    ) -> Result<ast::Expr, Diagnostic> {
        // A newline may follow an unfinished operator, opening delimiter, or '='.
        self.newlines();
        let mut left = self.prefix(multiline, records)?;
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
            let right = self.expr(precedence + 1, multiline, records)?;
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

    fn prefix(&mut self, multiline: bool, records: bool) -> Result<ast::Expr, Diagnostic> {
        if self.word("if") {
            let value = self.if_expr()?;
            return self.fields(value);
        }
        if self.word("match") {
            let value = self.match_expr()?;
            return self.fields(value);
        }
        let token = self.bump();
        let mut span = token.span;
        let kind =
            match token.kind {
                Kind::Number(number) => ast::ExprKind::Int(number.parse().map_err(|_| {
                    Diagnostic::new(span, "integer literal is outside the Int range")
                })?),
                Kind::Text(value) => ast::ExprKind::Text(value),
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
                    let types = self.type_args()?;
                    if !types.is_empty() {
                        span.end = self.tokens[self.pos - 1].span.end;
                    }
                    if self.eat(&Kind::LParen) {
                        let mut args = Vec::new();
                        self.newlines();
                        while !self.at(&Kind::RParen) {
                            args.push(self.expr(0, true, true)?);
                            self.newlines();
                            if !self.eat(&Kind::Comma) {
                                break;
                            }
                            self.newlines();
                        }
                        span.end = self.expect(Kind::RParen, "')'")?.span.end;
                        ast::ExprKind::Call { path, types, args }
                    } else if records && self.eat(&Kind::LBrace) {
                        let ty = ast::TypeRef {
                            path,
                            args: types,
                            span,
                        };
                        let mut fields = Vec::new();
                        self.newlines();
                        while !self.at(&Kind::RBrace) {
                            let (name, _) = self.name()?;
                            self.expect(Kind::Assign, "'=' after the field name")?;
                            fields.push((name, self.expr(0, false, true)?));
                            self.eat(&Kind::Comma);
                            self.newlines();
                        }
                        span.end = self.expect(Kind::RBrace, "'}'")?.span.end;
                        ast::ExprKind::Record { ty, fields }
                    } else {
                        if !types.is_empty() {
                            return Err(self
                                .error("expected a call or record literal after type arguments"));
                        }
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
                    let value = self.expr(7, multiline, records)?;
                    span.end = value.span.end;
                    ast::ExprKind::Unary(operator, Box::new(value))
                }
                Kind::LParen => {
                    let value = self.expr(0, true, true)?;
                    self.newlines();
                    span.end = self.expect(Kind::RParen, "')'")?.span.end;
                    return self.fields(ast::Expr { span, ..value });
                }
                Kind::LBrace => {
                    self.pos -= 1;
                    let (body, end) = self.block()?;
                    span.end = end.end;
                    ast::ExprKind::Block(body)
                }
                _ => return Err(Diagnostic::new(span, "expected an expression")),
            };
        self.fields(ast::Expr { kind, span })
    }

    fn fields(&mut self, mut value: ast::Expr) -> Result<ast::Expr, Diagnostic> {
        while self.eat(&Kind::Dot) {
            let (field, end) = self.name()?;
            let span = Span {
                end: end.end,
                ..value.span
            };
            value = ast::Expr {
                kind: ast::ExprKind::Field(Box::new(value), field),
                span,
            };
        }
        Ok(value)
    }

    fn pattern(&mut self) -> Result<ast::Pattern, Diagnostic> {
        if self.eat_word("_") {
            return Ok(ast::Pattern::Wildcard);
        }
        let (path, _) = self.path()?;
        if !self.eat(&Kind::LParen) {
            return Ok(ast::Pattern::Name(path));
        }
        let mut bindings = Vec::new();
        self.newlines();
        while !self.at(&Kind::RParen) {
            bindings.push(if self.eat_word("_") {
                None
            } else {
                Some(self.name()?.0)
            });
            if self.at(&Kind::LParen) || self.at(&Kind::Dot) || self.at(&Kind::LBrace) {
                return Err(self.error(
                    "nested patterns are not supported; bind the value and match it separately",
                ));
            }
            self.newlines();
            if !self.eat(&Kind::Comma) {
                break;
            }
            self.newlines();
        }
        self.expect(Kind::RParen, "')' after pattern bindings")?;
        Ok(ast::Pattern::Variant { path, bindings })
    }

    fn match_expr(&mut self) -> Result<ast::Expr, Diagnostic> {
        let mut span = self.bump().span;
        let value = Box::new(self.expr(0, false, false)?);
        self.newlines();
        self.expect(Kind::LBrace, "'{' after the matched value")?;
        self.newlines();
        let mut arms = Vec::new();
        while !self.at(&Kind::RBrace) {
            let mut span = self.peek().span;
            let pattern = self.pattern()?;
            self.expect(Kind::Arrow, "'=>' after the pattern")?;
            self.newlines();
            let braced = self.at(&Kind::LBrace);
            let body = if braced {
                let (body, end) = self.block()?;
                span.end = end.end;
                body
            } else {
                let value = self.expr(0, false, true)?;
                span.end = value.span.end;
                vec![ast::Stmt {
                    span: value.span,
                    kind: ast::StmtKind::Expr(value),
                }]
            };
            arms.push(ast::MatchArm {
                pattern,
                body,
                span,
            });
            if !self.eat(&Kind::Comma)
                && !braced
                && !self.at(&Kind::Newline)
                && !self.at(&Kind::RBrace)
            {
                return Err(self.error("expected a comma, newline, or '}' after the match arm"));
            }
            self.newlines();
        }
        span.end = self.bump().span.end;
        Ok(ast::Expr {
            kind: ast::ExprKind::Match { value, arms },
            span,
        })
    }

    fn if_expr(&mut self) -> Result<ast::Expr, Diagnostic> {
        let mut span = self.bump().span;
        // The following brace starts the control-flow body. Record literals in
        // conditions can be parenthesized to make their boundary unambiguous.
        let condition = Box::new(self.expr(0, false, false)?);
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
            | "intrinsic"
            | "record"
            | "enum"
            | "match"
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
        assert_eq!(function.params[0].ty.path, ["Int"]);
        assert_eq!(function.result.as_ref().unwrap().path, ["Int"]);
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
            "pub test fn f() {}",
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

    #[test]
    fn generic_data_and_computed_field_access() {
        let file = parse(
            0,
            "pub record Pair[A, B] { first A second B }\n\
            enum Outcome[T] { Empty Value(T) Partial(T, Int) }\n\
            fn identity[T](value T) T { value }\n\
            fn example() Int {\n\
                let pair Pair[Int, Pair[Bool, Int]] = Pair {\n\
                    second = Pair { first = true second = 2 }\n\
                    first = 1\n\
                }\n\
                identity[Pair[Int, Pair[Bool, Int]]](pair).second.second\n\
            }",
        )
        .unwrap();
        assert!(file.data[0].public);
        assert_eq!(file.data[0].parameters, ["A", "B"]);
        let ast::DataKind::Record(fields) = &file.data[0].kind else {
            panic!()
        };
        assert_eq!(fields[1].ty.path, ["B"]);
        let ast::DataKind::Enum(variants) = &file.data[1].kind else {
            panic!()
        };
        assert_eq!(variants[2].fields.len(), 2);
        assert_eq!(file.functions[0].parameters, ["T"]);
        let ast::StmtKind::Let {
            annotation: Some(ty),
            value,
            ..
        } = &file.functions[1].body[0].kind
        else {
            panic!()
        };
        assert_eq!(ty.args[1].args[1].path, ["Int"]);
        let ast::ExprKind::Record { fields, .. } = &value.kind else {
            panic!()
        };
        assert_eq!(fields[0].0, "second");
        assert_eq!(fields[1].0, "first");
        let ast::StmtKind::Expr(value) = &file.functions[1].body[1].kind else {
            panic!()
        };
        let ast::ExprKind::Field(receiver, name) = &value.kind else {
            panic!()
        };
        assert_eq!(name, "second");
        assert!(matches!(receiver.kind, ast::ExprKind::Field(_, _)));
    }

    #[test]
    fn constructors_match_patterns_and_blocks() {
        let file = parse(
            0,
            "fn example() Int {\n\
            let value = Outcome.Value[Int](3)\n\
            match value {\n\
                Outcome.Value(n) => n,\n\
                Partial(n, _) => { let result = n + 1\n result }\n\
                Empty => 0\n\
                _ => -1\n\
            }\n\
        }\n\
        fn binding() Int { match Outcome.Empty { value => { 1 } } }\n\
        fn block() Int { { 42 } }",
        )
        .unwrap();
        let ast::StmtKind::Let { value, .. } = &file.functions[0].body[0].kind else {
            panic!()
        };
        let ast::ExprKind::Call { path, types, args } = &value.kind else {
            panic!()
        };
        assert_eq!(path, &["Outcome", "Value"]);
        assert_eq!(types[0].path, ["Int"]);
        assert_eq!(args.len(), 1);
        let ast::StmtKind::Expr(value) = &file.functions[0].body[1].kind else {
            panic!()
        };
        let ast::ExprKind::Match { arms, .. } = &value.kind else {
            panic!()
        };
        assert_eq!(arms.len(), 4);
        assert_eq!(arms[1].body.len(), 2);
        assert!(matches!(arms[3].pattern, ast::Pattern::Wildcard));
        let error = parse(0, "fn f() { match x { Value(Empty()) => 0 } }").unwrap_err();
        assert!(error.message.contains("nested patterns"));
    }

    #[test]
    fn record_literals_do_not_steal_control_flow_bodies() {
        let file = parse(
            0,
            "fn f(flag Bool) Int {\n\
            var done = false\n\
            while flag { done = true }\n\
            if flag { done = true }\n\
            if (Pair { first = done second = 1 }).first { 1 } else { 0 }\n\
        }",
        )
        .unwrap();
        let ast::StmtKind::While { condition, .. } = &file.functions[0].body[1].kind else {
            panic!()
        };
        assert!(matches!(condition.kind, ast::ExprKind::Name(_)));
        let ast::StmtKind::Expr(value) = &file.functions[0].body[3].kind else {
            panic!()
        };
        let ast::ExprKind::If { condition, .. } = &value.kind else {
            panic!()
        };
        assert!(matches!(condition.kind, ast::ExprKind::Field(_, _)));
    }

    #[test]
    fn text_literals_and_private_intrinsic_declarations() {
        let file = parse(
            0,
            r#"intrinsic fn copy[T](value T) T
intrinsic fn collect()
fn example() Text { "hé\n\t\"\\\u{1f9f5}" }
"#,
        )
        .unwrap();
        assert!(file.functions[0].intrinsic);
        assert_eq!(file.functions[0].parameters, ["T"]);
        assert!(file.functions[0].body.is_empty());
        assert!(file.functions[1].result.is_none());
        let ast::StmtKind::Expr(value) = &file.functions[2].body[0].kind else {
            panic!()
        };
        let ast::ExprKind::Text(value) = &value.kind else {
            panic!()
        };
        assert_eq!(value, "hé\n\t\"\\🧵");
        for source in [
            "pub intrinsic fn f()",
            "test intrinsic fn f()",
            "intrinsic fn f() {}",
            "intrinsic fn f() requires true",
            "intrinsic fn f() Unit",
            "fn f() Text { \"unterminated }",
            "fn f() Text { \"line\nbreak\" }",
            r#"fn f() Text { "\x01" }"#,
            r#"fn f() Text { "\u{110000}" }"#,
        ] {
            assert!(parse(0, source).is_err(), "unexpectedly accepted {source}");
        }
    }
}
