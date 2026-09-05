//! Scalar checking and explicit binding for the native bootstrap slice.

use std::collections::HashMap;

use crate::model::{Binary, Diagnostic, PackageFile, Span, Type, Unary, ast, checked as c};

#[derive(Clone)]
struct Signature {
    file: usize,
    function: usize,
    params: Vec<Type>,
    result: Type,
    test_only: bool,
}

fn named_type(name: &str, span: Span) -> Result<Type, Diagnostic> {
    match name {
        "Int" => Ok(Type::Int),
        "Bool" => Ok(Type::Bool),
        "Unit" => Err(Diagnostic::new(
            span,
            "omit Unit; it is not a source type in this slice",
        )),
        _ => Err(Diagnostic::new(
            span,
            format!("type `{name}` is not supported by the native seed"),
        )),
    }
}

pub fn check(
    files: &[PackageFile],
    root_package: &str,
    test_mode: bool,
) -> Result<c::Program, Diagnostic> {
    let mut signatures: Vec<Signature> = Vec::new();
    for (file, source) in files.iter().enumerate() {
        for (function, item) in source.syntax.functions.iter().enumerate() {
            if !test_mode && (source.test_only || item.test) {
                continue;
            }
            let params = item
                .params
                .iter()
                .map(|p| named_type(&p.ty, p.span))
                .collect::<Result<Vec<_>, _>>()?;
            let result = item
                .result
                .as_ref()
                .map(|t| named_type(t, item.span))
                .transpose()?
                .unwrap_or(Type::Unit);
            if (item.test || (source.package == root_package && item.name == "main"))
                && (!params.is_empty() || result != Type::Unit)
            {
                return Err(Diagnostic::new(
                    item.span,
                    "main and test functions take no parameters and omit the return type",
                ));
            }
            if signatures.iter().any(|s| {
                files[s.file].package == source.package
                    && files[s.file].syntax.functions[s.function].name == item.name
                    && s.params == params
            }) {
                return Err(Diagnostic::new(
                    item.span,
                    "duplicate overload; return type alone cannot distinguish functions",
                ));
            }
            signatures.push(Signature {
                file,
                function,
                params,
                result,
                test_only: source.test_only || item.test,
            });
        }
    }
    // Imports are declarations of the package, not an order-sensitive lookup fallback.
    for source in files {
        if source.test_only && !test_mode {
            continue;
        }
        for import in &source.syntax.imports {
            let (name, package) = split_path(&import.path);
            if !signatures.iter().any(|s| {
                let f = &files[s.file].syntax.functions[s.function];
                files[s.file].package == package
                    && f.name == name
                    && (f.public || source.package == package)
                    && (!s.test_only || source.test_only)
            }) {
                return Err(Diagnostic::new(
                    import.span,
                    "import does not name an accessible production function",
                ));
            }
        }
    }
    let mut program = c::Program {
        functions: Vec::new(),
        entry: None,
        tests: Vec::new(),
        exports: Vec::new(),
    };
    for (id, sig) in signatures.iter().enumerate() {
        let source = &files[sig.file];
        let item = &source.syntax.functions[sig.function];
        let mut checker = Checker {
            files,
            signatures: &signatures,
            current: sig,
            scopes: vec![HashMap::new()],
            locals: Vec::new(),
        };
        for (param, ty) in item.params.iter().zip(&sig.params) {
            checker.bind(&param.name, *ty, false, param.span)?;
        }
        for contract in item.requires.iter().chain(&item.ensures) {
            scalar_contract(contract)?;
        }
        let requires = item
            .requires
            .iter()
            .map(|e| checker.expect(e, Type::Bool))
            .collect::<Result<Vec<_>, _>>()?;
        let (body, _) = checker.block(&item.body, Some(sig.result))?;
        if body.falls_through && sig.result != Type::Unit && body.tail.is_none() {
            return Err(Diagnostic::new(
                item.span,
                "not every normal path returns a value",
            ));
        }
        // The contract-only result slot is never emitted as runtime storage.
        let result_slot = checker.locals.len();
        checker.locals.push(sig.result);
        checker.scopes[0].insert(
            "result".into(),
            Binding {
                local: result_slot,
                mutable: false,
            },
        );
        let ensures = item
            .ensures
            .iter()
            .map(|e| checker.expect(e, Type::Bool))
            .collect::<Result<Vec<_>, _>>()?;
        checker.locals.pop();
        let function = c::Function {
            name: if source.package.is_empty() {
                item.name.clone()
            } else {
                format!("{}.{}", source.package, item.name)
            },
            params: sig.params.clone(),
            result: sig.result,
            locals: checker.locals,
            requires,
            body,
            span: item.span,
        };
        if !ensures.is_empty() {
            crate::proof::prove(&function, &ensures, result_slot)?;
        }
        if source.package == root_package && item.name == "main" && !sig.test_only {
            program.entry = Some(id);
        }
        if item.test {
            program.tests.push(id);
        }
        if source.package == root_package && item.public && !sig.test_only {
            program.exports.push(id);
        }
        program.functions.push(function);
    }
    Ok(program)
}

fn scalar_contract(expr: &ast::Expr) -> Result<(), Diagnostic> {
    match &expr.kind {
        ast::ExprKind::Call(..) | ast::ExprKind::If { .. } => Err(Diagnostic::new(
            expr.span,
            "the seed supports only scalar, call-free contract predicates",
        )),
        ast::ExprKind::Unary(_, value) => scalar_contract(value),
        ast::ExprKind::Binary(_, a, b) => {
            scalar_contract(a)?;
            scalar_contract(b)
        }
        _ => Ok(()),
    }
}

fn split_path(path: &[String]) -> (&str, String) {
    (
        path.last().map(String::as_str).unwrap_or(""),
        path[..path.len().saturating_sub(1)].join("."),
    )
}

#[derive(Clone, Copy)]
struct Binding {
    local: usize,
    mutable: bool,
}

struct Checker<'a> {
    files: &'a [PackageFile],
    signatures: &'a [Signature],
    current: &'a Signature,
    scopes: Vec<HashMap<String, Binding>>,
    locals: Vec<Type>,
}

impl Checker<'_> {
    fn bind(
        &mut self,
        name: &str,
        ty: Type,
        mutable: bool,
        span: Span,
    ) -> Result<usize, Diagnostic> {
        if name == "result" {
            return Err(Diagnostic::new(
                span,
                "`result` is reserved for postconditions",
            ));
        }
        let scope = self.scopes.last_mut().unwrap();
        if scope.contains_key(name) {
            return Err(Diagnostic::new(span, format!("duplicate local `{name}`")));
        }
        if ty == Type::Unit {
            return Err(Diagnostic::new(span, "a binding must have a value"));
        }
        let local = self.locals.len();
        self.locals.push(ty);
        scope.insert(name.to_owned(), Binding { local, mutable });
        Ok(local)
    }

    fn local(&self, name: &str, span: Span) -> Result<Binding, Diagnostic> {
        self.scopes
            .iter()
            .rev()
            .find_map(|s| s.get(name).copied())
            .ok_or_else(|| Diagnostic::new(span, format!("unknown local `{name}`")))
    }

    fn expect(&mut self, expr: &ast::Expr, ty: Type) -> Result<c::Expr, Diagnostic> {
        let value = self.expr(expr, Some(ty))?;
        if value.ty != ty && expr_falls(&value) {
            return Err(Diagnostic::new(
                expr.span,
                format!("expected {ty:?}, found {:?}", value.ty),
            ));
        }
        Ok(value)
    }

    fn block(
        &mut self,
        block: &ast::Block,
        expected: Option<Type>,
    ) -> Result<(c::Block, Type), Diagnostic> {
        self.scopes.push(HashMap::new());
        let mut statements = Vec::new();
        let mut tail = None;
        let mut falls_through = true;
        let mut ty = Type::Unit;
        for (index, stmt) in block.iter().enumerate() {
            use ast::StmtKind as A;
            use c::StmtKind as C;
            if let A::Expr(expr) = &stmt.kind {
                if index + 1 == block.len() {
                    let value = self.expr(expr, expected)?;
                    ty = value.ty;
                    falls_through &= expr_falls(&value);
                    tail = Some(Box::new(value));
                    continue;
                }
            }
            let kind = match &stmt.kind {
                A::Let {
                    name,
                    mutable,
                    annotation,
                    value,
                } => {
                    let annotation = annotation
                        .as_ref()
                        .map(|t| named_type(t, stmt.span))
                        .transpose()?;
                    let value = self.expr(value, annotation)?;
                    let local = self.bind(name, value.ty, *mutable, stmt.span)?;
                    falls_through &= expr_falls(&value);
                    C::Let { local, value }
                }
                A::Assign { name, value } => {
                    let binding = self.local(name, stmt.span)?;
                    if !binding.mutable {
                        return Err(Diagnostic::new(
                            stmt.span,
                            format!("`{name}` is immutable; use var for a mutable local"),
                        ));
                    }
                    let value = self.expect(value, self.locals[binding.local])?;
                    falls_through &= expr_falls(&value);
                    C::Assign {
                        local: binding.local,
                        value,
                    }
                }
                A::Return(expr) => {
                    let value = expr
                        .as_ref()
                        .map(|e| self.expect(e, self.current.result))
                        .transpose()?;
                    if value.is_none() && self.current.result != Type::Unit {
                        return Err(Diagnostic::new(stmt.span, "return requires a value"));
                    }
                    falls_through = false;
                    C::Return(value)
                }
                A::Assert(expr) => C::Assert(self.expect(expr, Type::Bool)?),
                A::Discard(expr) => C::Discard(self.expr(expr, None)?),
                A::Expr(expr) => {
                    let value = self.expect(expr, Type::Unit)?;
                    falls_through &= expr_falls(&value);
                    C::Expr(value)
                }
                A::While { condition, body } => {
                    let condition = self.expect(condition, Type::Bool)?;
                    let (body, _) = self.block(body, Some(Type::Unit))?;
                    C::While { condition, body }
                }
            };
            statements.push(c::Stmt {
                kind,
                span: stmt.span,
            });
        }
        self.scopes.pop();
        if falls_through {
            if let Some(expected) = expected {
                if ty != expected {
                    return Err(Diagnostic::new(
                        block.last().map(|s| s.span).unwrap_or_default(),
                        format!(
                            "expected {expected:?} tail, found {ty:?}; use discard to ignore a value"
                        ),
                    ));
                }
            }
        }
        Ok((
            c::Block {
                statements,
                tail,
                falls_through,
            },
            ty,
        ))
    }

    fn expr(&mut self, expr: &ast::Expr, expected: Option<Type>) -> Result<c::Expr, Diagnostic> {
        use ast::ExprKind as A;
        use c::ExprKind as C;
        let (kind, ty) = match &expr.kind {
            A::Int(value) => (C::Int(*value), Type::Int),
            A::Bool(value) => (C::Bool(*value), Type::Bool),
            A::Name(path) => {
                if path.len() != 1 {
                    return Err(Diagnostic::new(expr.span, "qualified names must be called"));
                }
                let binding = self.local(&path[0], expr.span)?;
                (C::Local(binding.local), self.locals[binding.local])
            }
            A::Unary(op, value) => {
                let ty = if *op == Unary::Not {
                    Type::Bool
                } else {
                    Type::Int
                };
                (C::Unary(*op, Box::new(self.expect(value, ty)?)), ty)
            }
            A::Binary(op, left, right) => {
                let left = self.expr(left, None)?;
                let argument = match op {
                    Binary::And | Binary::Or => Type::Bool,
                    Binary::Eq | Binary::Ne => left.ty,
                    _ => Type::Int,
                };
                if left.ty != argument || left.ty == Type::Unit {
                    return Err(Diagnostic::new(left.span, "invalid operator operand type"));
                }
                let right = self.expect(right, argument)?;
                let ty = match op {
                    Binary::Add | Binary::Sub | Binary::Mul | Binary::Div | Binary::Rem => {
                        Type::Int
                    }
                    _ => Type::Bool,
                };
                (C::Binary(*op, Box::new(left), Box::new(right)), ty)
            }
            A::Call(path, arguments) => {
                if path.len() == 1 && self.scopes.iter().any(|scope| scope.contains_key(&path[0])) {
                    return Err(Diagnostic::new(
                        expr.span,
                        format!("local `{}` is not callable", path[0]),
                    ));
                }
                let args = arguments
                    .iter()
                    .map(|a| self.expr(a, None))
                    .collect::<Result<Vec<_>, _>>()?;
                let current_package = &self.files[self.current.file].package;
                let (name, qualified) = split_path(path);
                let imports = self
                    .files
                    .iter()
                    .filter(|f| {
                        &f.package == current_package && (!f.test_only || self.current.test_only)
                    })
                    .flat_map(|f| &f.syntax.imports)
                    .filter(|i| i.path.last().is_some_and(|n| n == name));
                let mut packages = Vec::new();
                if path.len() == 1 {
                    packages.push(current_package.clone());
                } else if &qualified == current_package {
                    packages.push(qualified.clone());
                }
                for import in imports {
                    let (_, package) = split_path(&import.path);
                    if path.len() == 1 || qualified == package {
                        packages.push(package);
                    }
                }
                packages.sort();
                packages.dedup();
                let candidates: Vec<_> = self
                    .signatures
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| {
                        let source = &self.files[s.file];
                        let function = &source.syntax.functions[s.function];
                        function.name == name
                            && packages.contains(&source.package)
                            && (source.package == *current_package || function.public)
                            && (!s.test_only || self.current.test_only)
                            && s.params.iter().copied().eq(args.iter().map(|a| a.ty))
                    })
                    .collect();
                let (id, signature) = match candidates.as_slice() {
                    [only] => *only,
                    [] => {
                        return Err(Diagnostic::new(
                            expr.span,
                            format!(
                                "no accessible overload of `{}` matches these arguments",
                                path.join(".")
                            ),
                        ));
                    }
                    _ => {
                        return Err(Diagnostic::new(
                            expr.span,
                            format!(
                                "ambiguous overload of `{}`; qualify the call",
                                path.join(".")
                            ),
                        ));
                    }
                };
                (C::Call(id, args), signature.result)
            }
            A::If {
                condition,
                then_body,
                else_body,
            } => {
                let condition = self.expect(condition, Type::Bool)?;
                let (then_body, then_ty) = self.block(then_body, expected)?;
                let other_expect = expected.or(then_body.falls_through.then_some(then_ty));
                let (else_body, else_ty) = if let Some(body) = else_body {
                    let (body, ty) = self.block(body, other_expect)?;
                    (Some(body), ty)
                } else {
                    (None, Type::Unit)
                };
                let ty = if then_body.falls_through {
                    then_ty
                } else {
                    else_ty
                };
                if else_body.as_ref().is_none_or(|b| b.falls_through) && ty != else_ty {
                    return Err(Diagnostic::new(
                        expr.span,
                        "if branches must produce the same type; a value-producing if needs else",
                    ));
                }
                (
                    C::If {
                        condition: Box::new(condition),
                        then_body,
                        else_body,
                    },
                    ty,
                )
            }
        };
        let checked = c::Expr {
            kind,
            ty,
            span: expr.span,
        };
        if expected.is_some_and(|e| e != ty) && expr_falls(&checked) {
            return Err(Diagnostic::new(
                expr.span,
                format!(
                    "expected {:?}, found {ty:?}; use discard to ignore a value",
                    expected.unwrap()
                ),
            ));
        }
        Ok(checked)
    }
}

fn expr_falls(expr: &c::Expr) -> bool {
    match &expr.kind {
        c::ExprKind::If {
            condition,
            then_body,
            else_body,
        } => {
            expr_falls(condition)
                && (then_body.falls_through || else_body.as_ref().is_none_or(|b| b.falls_through))
        }
        c::ExprKind::Unary(_, e) => expr_falls(e),
        c::ExprKind::Binary(Binary::And | Binary::Or, a, _) => expr_falls(a),
        c::ExprKind::Binary(_, a, b) => expr_falls(a) && expr_falls(b),
        c::ExprKind::Call(_, args) => args.iter().all(expr_falls),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn file(package: &str, source: &str, test_only: bool) -> PackageFile {
        PackageFile {
            package: package.into(),
            test_only,
            syntax: crate::parser::parse(0, source).unwrap(),
        }
    }
    fn rejects(source: &str) {
        assert!(
            check(&[file("", source, false)], "", false).is_err(),
            "{source}"
        );
    }

    #[test]
    fn contracts_need_proof_not_runtime_fallback() {
        rejects("fn wrong(x Int) Int ensures result > x { x }");
        rejects(
            "fn hidden(x Int) Int { x } fn unknown(x Int) Int ensures result == x { hidden(x) }",
        );
        rejects("fn unsupported(x Int) Int ensures result >= 0 { x * x }");
        rejects("fn dishonest(x Int) Int ensures result == 1 { 2 }");
        rejects("fn overflow(x Int) Int ensures result + 1 > result { x }");
        rejects("fn effectful() Bool { assert false\ntrue } fn invalid() requires effectful() {}");
    }

    #[test]
    fn proves_returns_under_preconditions_and_branch_facts() {
        let source = "fn identity(x Int) Int requires x > 0 ensures result > 0 ensures result == x { x }\nfn abs(x Int) Int ensures result >= 0 { if x >= 0 { x } else { -x } }\nfn next(x Int) Int ensures result > x { x + 1 }";
        check(&[file("", source, false)], "", false).unwrap();
        check(
            &[file(
                "",
                "fn same(x Bool) Bool ensures result == x { x }",
                false,
            )],
            "",
            false,
        )
        .unwrap();
    }

    #[test]
    fn type_and_temporary_rules() {
        rejects("fn main() { 1 }");
        rejects("fn main() { let x = true\nvar y = 1\ny = x }");
        rejects("fn main() { let x = 1\nx = 2 }");
        rejects("fn choose(x Int) Int { x } fn choose(x Int) Bool { true }");
        check(&[file("", "fn main() { discard 1 }", false)], "", false).unwrap();
    }

    #[test]
    fn visibility_and_test_helpers_are_separate() {
        let production = file("demo", "fn secret() Int { 7 }", false);
        let tests = file("demo", "test fn works() { assert secret() == 7 }", true);
        assert_eq!(
            check(&[production.clone(), tests], "demo", true)
                .unwrap()
                .tests
                .len(),
            1
        );
        let other = file(
            "other",
            "import demo.secret\nfn main() { discard secret() }",
            false,
        );
        assert!(check(&[production, other], "other", false).is_err());
        let prod = file("demo", "fn main() { helper() }", false);
        let helpers = file("demo", "fn helper() {}", true);
        assert!(check(&[prod, helpers], "demo", true).is_err());
    }

    #[test]
    fn imports_and_overloads_do_not_depend_on_file_order() {
        let imports = file("demo", "import util.pick", false);
        let app = file(
            "demo",
            "fn main() { assert pick(1) == 1\nassert pick(true) }",
            false,
        );
        let util = file(
            "util",
            "pub fn pick(x Int) Int { x } pub fn pick(x Bool) Bool { x }",
            false,
        );
        check(&[app.clone(), imports.clone(), util.clone()], "demo", false).unwrap();
        let conflict = file("demo", "fn pick(x Int) Int { x }", false);
        assert!(check(&[app, imports, util, conflict], "demo", false).is_err());
    }
}
