//! Binding, typed data, and bounded specialization for the native seed.

use std::collections::{HashMap, HashSet};

use crate::model::{
    Binary, Diagnostic, PackageFile, Primitive, Span, Type, Unary, ast, checked as c,
};

#[derive(Clone)]
struct Signature {
    file: usize,
    function: usize,
    params: Vec<Type>,
    result: Type,
    test_only: bool,
}

#[derive(Clone)]
struct DataInstance {
    declaration: usize,
    arguments: Vec<Type>,
}

struct Environment<'a> {
    files: &'a [PackageFile],
    signatures: Vec<Signature>,
    declarations: Vec<(usize, usize)>,
    types: Vec<c::Data>,
    lists: Vec<Type>,
    data_instances: Vec<DataInstance>,
    building: Vec<usize>,
    functions: Vec<(usize, Vec<Type>)>,
}

fn split_path(path: &[String]) -> (&str, String) {
    (
        path.last().map(String::as_str).unwrap_or(""),
        path[..path.len().saturating_sub(1)].join("."),
    )
}

fn qualified(package: &str, name: &str) -> String {
    if package.is_empty() {
        name.into()
    } else {
        format!("{package}.{name}")
    }
}

fn parameters(names: &[String], span: Span) -> Result<HashMap<String, Type>, Diagnostic> {
    let mut result = HashMap::new();
    for (index, name) in names.iter().enumerate() {
        if matches!(
            name.as_str(),
            "Int" | "Bool" | "Text" | "Bytes" | "List" | "Unit"
        ) || result
            .insert(name.clone(), Type::Parameter(index))
            .is_some()
        {
            return Err(Diagnostic::new(
                span,
                "duplicate or reserved type parameter",
            ));
        }
    }
    Ok(result)
}

pub fn check(
    files: &[PackageFile],
    root_package: &str,
    test_mode: bool,
) -> Result<c::Program, Diagnostic> {
    let mut env = Environment {
        files,
        signatures: Vec::new(),
        declarations: Vec::new(),
        types: Vec::new(),
        lists: Vec::new(),
        data_instances: Vec::new(),
        building: Vec::new(),
        functions: Vec::new(),
    };
    for (file, source) in files.iter().enumerate() {
        if source.test_only && !test_mode {
            continue;
        }
        for (index, data) in source.syntax.data.iter().enumerate() {
            parameters(&data.parameters, data.span)?;
            if matches!(
                data.name.as_str(),
                "Int" | "Bool" | "Text" | "Bytes" | "List" | "Unit"
            ) || env.declarations.iter().any(|&(f, d)| {
                files[f].package == source.package && files[f].syntax.data[d].name == data.name
            }) {
                return Err(Diagnostic::new(
                    data.span,
                    "duplicate or reserved data declaration",
                ));
            }
            env.declarations.push((file, index));
        }
        for (function, item) in source.syntax.functions.iter().enumerate() {
            if item.test && !test_mode {
                continue;
            }
            if item.intrinsic
                && (!source.trusted_std || item.public || item.test || source.test_only)
            {
                return Err(Diagnostic::new(
                    item.span,
                    "intrinsics are private declarations of the trusted standard library",
                ));
            }
            parameters(&item.parameters, item.span)?;
            env.signatures.push(Signature {
                file,
                function,
                params: Vec::new(),
                result: Type::Unit,
                test_only: source.test_only || item.test,
            });
        }
    }
    env.validate_imports(test_mode)?;
    // Validate unused declarations as well as instances reached by this build.
    for id in 0..env.declarations.len() {
        let (file, index) = env.declarations[id];
        let data = &files[file].syntax.data[index];
        env.data(
            id,
            (0..data.parameters.len()).map(Type::Parameter).collect(),
            data.span,
        )?;
    }
    for id in 0..env.signatures.len() {
        let sig = env.signatures[id].clone();
        let item = &files[sig.file].syntax.functions[sig.function];
        let params = parameters(&item.parameters, item.span)?;
        let arguments = item
            .params
            .iter()
            .map(|p| env.resolve(&p.ty, sig.file, sig.test_only, &params))
            .collect::<Result<Vec<_>, _>>()?;
        let result = item
            .result
            .as_ref()
            .map(|r| env.resolve(r, sig.file, sig.test_only, &params))
            .transpose()?
            .unwrap_or(Type::Unit);
        if (item.test || (files[sig.file].package == root_package && item.name == "main"))
            && (!arguments.is_empty() || result != Type::Unit || !item.parameters.is_empty())
        {
            return Err(Diagnostic::new(
                item.span,
                "main and test functions take no parameters and omit the return type",
            ));
        }
        if env.signatures[..id].iter().any(|other| {
            let f = &files[other.file].syntax.functions[other.function];
            files[other.file].package == files[sig.file].package
                && f.name == item.name
                && f.parameters.len() == item.parameters.len()
                && other.params == arguments
        }) {
            return Err(Diagnostic::new(
                item.span,
                "duplicate overload; return type alone cannot distinguish functions",
            ));
        }
        env.signatures[id].params = arguments;
        env.signatures[id].result = result;
    }
    for id in 0..env.declarations.len() {
        env.validate_refinement(id)?;
    }
    for id in 0..env.signatures.len() {
        let sig = &env.signatures[id];
        let item = &files[sig.file].syntax.functions[sig.function];
        let args = (0..item.parameters.len()).map(Type::Parameter).collect();
        env.function(id, args, false)?;
    }
    let mut program = c::Program {
        types: Vec::new(),
        lists: Vec::new(),
        functions: Vec::new(),
        entry: None,
        tests: Vec::new(),
        exports: Vec::new(),
    };
    for id in 0..env.signatures.len() {
        let sig = env.signatures[id].clone();
        let source = &files[sig.file];
        let item = &source.syntax.functions[sig.function];
        if !item.parameters.is_empty() {
            continue;
        }
        let entry = source.package == root_package && item.name == "main" && !sig.test_only;
        let export = source.package == root_package && item.public && !sig.test_only;
        if entry || export || item.test {
            let instance = env.schedule(id, Vec::new(), item.span)?;
            if entry {
                program.entry = Some(instance);
            }
            if export {
                program.exports.push(instance);
            }
            if item.test {
                program.tests.push(instance);
            }
        }
    }
    let mut next = 0;
    while next < env.functions.len() {
        let (id, args) = env.functions[next].clone();
        program.functions.push(env.function(id, args, true)?);
        next += 1;
    }
    program.types = env.types;
    program.lists = env.lists;
    Ok(program)
}

impl Environment<'_> {
    fn validate_imports(&self, test_mode: bool) -> Result<(), Diagnostic> {
        for source in self.files {
            if source.test_only && !test_mode {
                continue;
            }
            for import in &source.syntax.imports {
                let (name, package) = split_path(&import.path);
                let function = self.signatures.iter().any(|s| {
                    let item = &self.files[s.file].syntax.functions[s.function];
                    self.files[s.file].package == package
                        && item.name == name
                        && (item.public || source.package == package)
                        && (!s.test_only || source.test_only)
                });
                let data = self.declarations.iter().any(|&(file, index)| {
                    let item = &self.files[file].syntax.data[index];
                    self.files[file].package == package
                        && item.name == name
                        && (item.public || source.package == package)
                        && (!self.files[file].test_only || source.test_only)
                });
                if !function && !data {
                    return Err(Diagnostic::new(
                        import.span,
                        "import does not name an accessible declaration",
                    ));
                }
            }
        }
        Ok(())
    }

    fn packages(&self, path: &[String], file: usize, test_only: bool) -> Vec<String> {
        let current = &self.files[file].package;
        let (name, qualified) = split_path(path);
        let mut packages = Vec::new();
        if path.len() == 1 {
            packages.push(current.clone());
        } else if &qualified == current {
            packages.push(qualified.clone());
        }
        for import in self
            .files
            .iter()
            .filter(|f| &f.package == current && (!f.test_only || test_only))
            .flat_map(|f| &f.syntax.imports)
            .filter(|i| i.path.last().is_some_and(|n| n == name))
        {
            let (_, package) = split_path(&import.path);
            if path.len() == 1 || qualified == package {
                packages.push(package);
            }
        }
        packages.sort();
        packages.dedup();
        packages
    }

    fn data_name(
        &self,
        path: &[String],
        file: usize,
        test_only: bool,
        span: Span,
    ) -> Result<usize, Diagnostic> {
        let (name, _) = split_path(path);
        let packages = self.packages(path, file, test_only);
        let candidates: Vec<_> = self
            .declarations
            .iter()
            .enumerate()
            .filter(|(_, (f, d))| {
                let source = &self.files[*f];
                let data = &source.syntax.data[*d];
                data.name == name
                    && packages.contains(&source.package)
                    && (source.package == self.files[file].package || data.public)
                    && (!source.test_only || test_only)
            })
            .map(|(id, _)| id)
            .collect();
        match candidates.as_slice() {
            [id] => Ok(*id),
            [] => Err(Diagnostic::new(
                span,
                format!("unknown or inaccessible type `{}`", path.join(".")),
            )),
            _ => Err(Diagnostic::new(span, "ambiguous type; qualify its name")),
        }
    }

    fn resolve(
        &mut self,
        reference: &ast::TypeRef,
        file: usize,
        test_only: bool,
        parameters: &HashMap<String, Type>,
    ) -> Result<Type, Diagnostic> {
        if reference.path.len() == 1 {
            let name = &reference.path[0];
            if name == "List" {
                let [element] = reference.args.as_slice() else {
                    return Err(Diagnostic::new(
                        reference.span,
                        "List requires exactly one element type",
                    ));
                };
                let element = self.resolve(element, file, test_only, parameters)?;
                return self.list(element, reference.span);
            }
            let builtin = match name.as_str() {
                "Int" => Some(Type::Int),
                "Bool" => Some(Type::Bool),
                "Text" => Some(Type::Text),
                "Bytes" => Some(Type::Bytes),
                "Unit" => {
                    return Err(Diagnostic::new(
                        reference.span,
                        "omit Unit; it is not a source type",
                    ));
                }
                _ => parameters.get(name).copied(),
            };
            if let Some(ty) = builtin {
                if !reference.args.is_empty() {
                    return Err(Diagnostic::new(
                        reference.span,
                        "this type takes no arguments",
                    ));
                }
                return Ok(ty);
            }
        }
        let id = self.data_name(&reference.path, file, test_only, reference.span)?;
        let arguments = reference
            .args
            .iter()
            .map(|r| self.resolve(r, file, test_only, parameters))
            .collect::<Result<Vec<_>, _>>()?;
        self.data(id, arguments, reference.span)
    }

    fn list(&mut self, element: Type, span: Span) -> Result<Type, Diagnostic> {
        if self.type_depth(element) >= 64 {
            return Err(Diagnostic::new(
                span,
                "type specialization depth exceeded; generic expansion must be finite",
            ));
        }
        if let Some(id) = self.lists.iter().position(|t| *t == element) {
            return Ok(Type::List(id));
        }
        let id = self.lists.len();
        self.lists.push(element);
        Ok(Type::List(id))
    }

    fn data(
        &mut self,
        declaration: usize,
        arguments: Vec<Type>,
        span: Span,
    ) -> Result<Type, Diagnostic> {
        let (file, index) = self.declarations[declaration];
        let source = &self.files[file];
        let item = source.syntax.data[index].clone();
        if arguments.len() != item.parameters.len() {
            return Err(Diagnostic::new(
                span,
                format!(
                    "type `{}` needs {} type arguments",
                    item.name,
                    item.parameters.len()
                ),
            ));
        }
        if arguments.iter().any(|t| self.type_depth(*t) >= 64) {
            return Err(Diagnostic::new(
                span,
                "type specialization depth exceeded; generic expansion must be finite",
            ));
        }
        // No references in this slice: every declared field is part of the value layout.
        if self.building.contains(&declaration) || self.building.len() >= 64 {
            return Err(Diagnostic::new(
                span,
                "recursive by-value data layout is not supported",
            ));
        }
        if let Some(id) = self
            .data_instances
            .iter()
            .position(|i| i.declaration == declaration && i.arguments == arguments)
        {
            return Ok(Type::Data(id));
        }
        if self.types.len() >= 4096 {
            return Err(Diagnostic::new(span, "type specialization budget exceeded"));
        }
        self.building.push(declaration);
        let params = item
            .parameters
            .iter()
            .cloned()
            .zip(arguments.iter().copied())
            .collect();
        let mut names = HashSet::new();
        let kind = match &item.kind {
            ast::DataKind::Refined { base, predicate } => {
                if !item.parameters.is_empty() {
                    return Err(Diagnostic::new(
                        item.span,
                        "generic constrained declarations are not supported yet",
                    ));
                }
                if self.resolve(base, file, source.test_only, &params)? != Type::Int {
                    return Err(Diagnostic::new(
                        base.span,
                        "this slice only constrains Int directly; nested or shared constrained bases are unsupported",
                    ));
                }
                refinement_predicate(predicate)?;
                c::DataKind::Refined(Type::Int)
            }
            ast::DataKind::Record(fields) => {
                let mut result = Vec::new();
                for field in fields {
                    if !names.insert(&field.name) {
                        return Err(Diagnostic::new(field.span, "duplicate record field"));
                    }
                    result.push((
                        field.name.clone(),
                        self.resolve(&field.ty, file, source.test_only, &params)?,
                    ));
                }
                c::DataKind::Record(result)
            }
            ast::DataKind::Enum(variants) => {
                if variants.is_empty() {
                    return Err(Diagnostic::new(
                        item.span,
                        "an enum needs at least one variant",
                    ));
                }
                let mut result = Vec::new();
                for variant in variants {
                    if !names.insert(&variant.name) {
                        return Err(Diagnostic::new(variant.span, "duplicate enum variant"));
                    }
                    let fields = variant
                        .fields
                        .iter()
                        .map(|r| self.resolve(r, file, source.test_only, &params))
                        .collect::<Result<Vec<_>, _>>()?;
                    result.push((variant.name.clone(), fields));
                }
                c::DataKind::Enum(result)
            }
        };
        self.building.pop();
        let id = self.types.len();
        let mut name = qualified(&source.package, &item.name);
        if !arguments.is_empty() {
            name.push_str(&format!("{arguments:?}"));
        }
        self.types.push(c::Data { name, kind });
        self.data_instances.push(DataInstance {
            declaration,
            arguments,
        });
        Ok(Type::Data(id))
    }

    fn validate_refinement(&mut self, declaration: usize) -> Result<(), Diagnostic> {
        let (file, index) = self.declarations[declaration];
        let ast::DataKind::Refined { predicate, .. } =
            self.files[file].syntax.data[index].kind.clone()
        else {
            return Ok(());
        };
        let test_only = self.files[file].test_only;
        let mut checker = Checker {
            env: self,
            current: Signature {
                file,
                function: 0,
                params: Vec::new(),
                result: Type::Unit,
                test_only,
            },
            type_params: HashMap::new(),
            emit: false,
            scopes: vec![HashMap::new()],
            locals: Vec::new(),
        };
        checker.bind("self", Type::Int, false, predicate.span)?;
        checker.expect(&predicate, Type::Bool)?;
        Ok(())
    }

    fn widens(&self, actual: Type, expected: Type) -> bool {
        matches!(actual, Type::Data(id) if matches!(self.types[id].kind, c::DataKind::Refined(base) if base == expected))
    }

    fn result_language_items(
        &mut self,
        value: Type,
        span: Span,
    ) -> Result<(Type, Type, usize, usize, usize), Diagnostic> {
        let mut result = None;
        let mut error = None;
        for (id, &(file, index)) in self.declarations.iter().enumerate() {
            let source = &self.files[file];
            let item = &source.syntax.data[index];
            if source.trusted_std
                && source.package == "std.result"
                && item.public
                && !source.test_only
            {
                match item.name.as_str() {
                    "Result" if item.parameters.len() == 2 => result = Some(id),
                    "ConstraintError" if item.parameters.is_empty() => error = Some(id),
                    _ => (),
                }
            }
        }
        let (Some(result), Some(error)) = (result, error) else {
            return Err(Diagnostic::new(
                span,
                "constrained construction requires trusted std.result.Result and ConstraintError declarations",
            ));
        };
        let error_ty = self.data(error, Vec::new(), span)?;
        let Type::Data(error_id) = error_ty else {
            unreachable!()
        };
        let c::DataKind::Enum(error_variants) = &self.types[error_id].kind else {
            return Err(Diagnostic::new(
                span,
                "ConstraintError must be the source enum containing Rejected",
            ));
        };
        if error_variants.as_slice() != [("Rejected".into(), Vec::new())] {
            return Err(Diagnostic::new(
                span,
                "ConstraintError must contain only the empty Rejected variant",
            ));
        }
        let template = self.data(result, vec![Type::Parameter(0), Type::Parameter(1)], span)?;
        let Type::Data(template) = template else {
            unreachable!()
        };
        let c::DataKind::Enum(variants) = &self.types[template].kind else {
            return Err(Diagnostic::new(
                span,
                "Result must be the source enum with Ok(T) and Err(E)",
            ));
        };
        let ok = variants
            .iter()
            .position(|(name, fields)| name == "Ok" && fields == &[Type::Parameter(0)]);
        let err = variants
            .iter()
            .position(|(name, fields)| name == "Err" && fields == &[Type::Parameter(1)]);
        let (Some(ok), Some(err)) = (ok, err) else {
            return Err(Diagnostic::new(
                span,
                "Result must contain exactly Ok(T) and Err(E)",
            ));
        };
        if variants.len() != 2 {
            return Err(Diagnostic::new(
                span,
                "Result must contain exactly Ok(T) and Err(E)",
            ));
        }
        let result_ty = self.data(result, vec![value, error_ty], span)?;
        Ok((result_ty, error_ty, ok, err, 0))
    }

    fn substitute(&mut self, ty: Type, arguments: &[Type], span: Span) -> Result<Type, Diagnostic> {
        match ty {
            Type::Parameter(index) => Ok(arguments[index]),
            Type::List(id) => {
                let element = self.substitute(self.lists[id], arguments, span)?;
                self.list(element, span)
            }
            Type::Data(id) => {
                let instance = self.data_instances[id].clone();
                let args = instance
                    .arguments
                    .iter()
                    .map(|t| self.substitute(*t, arguments, span))
                    .collect::<Result<Vec<_>, _>>()?;
                self.data(instance.declaration, args, span)
            }
            ty => Ok(ty),
        }
    }

    fn infer(&self, pattern: Type, actual: Type, arguments: &mut [Option<Type>]) -> bool {
        match pattern {
            Type::Parameter(index) => match arguments[index] {
                Some(ty) => ty == actual,
                None => {
                    arguments[index] = Some(actual);
                    true
                }
            },
            Type::Data(p) => {
                let Type::Data(a) = actual else {
                    return false;
                };
                let p = &self.data_instances[p];
                let a = &self.data_instances[a];
                p.declaration == a.declaration
                    && p.arguments
                        .iter()
                        .zip(&a.arguments)
                        .all(|(p, a)| self.infer(*p, *a, arguments))
            }
            Type::List(p) => {
                let Type::List(a) = actual else {
                    return false;
                };
                self.infer(self.lists[p], self.lists[a], arguments)
            }
            _ => pattern == actual,
        }
    }

    fn concrete(&self, ty: Type) -> bool {
        match ty {
            Type::Parameter(_) => false,
            Type::List(id) => self.concrete(self.lists[id]),
            Type::Data(id) => self.data_instances[id]
                .arguments
                .iter()
                .all(|t| self.concrete(*t)),
            _ => true,
        }
    }

    fn type_depth(&self, ty: Type) -> usize {
        match ty {
            Type::List(id) => 1 + self.type_depth(self.lists[id]),
            Type::Data(id) => {
                1 + self.data_instances[id]
                    .arguments
                    .iter()
                    .map(|t| self.type_depth(*t))
                    .max()
                    .unwrap_or(0)
            }
            _ => 0,
        }
    }

    fn schedule(
        &mut self,
        declaration: usize,
        arguments: Vec<Type>,
        span: Span,
    ) -> Result<usize, Diagnostic> {
        if let Some(id) = self
            .functions
            .iter()
            .position(|(d, a)| *d == declaration && a == &arguments)
        {
            return Ok(id);
        }
        if self.functions.len() >= 1024 {
            return Err(Diagnostic::new(
                span,
                "function specialization budget exceeded; generic expansion must be finite",
            ));
        }
        let id = self.functions.len();
        self.functions.push((declaration, arguments));
        Ok(id)
    }

    fn intrinsic(
        &mut self,
        item: &ast::Function,
        sig: &Signature,
    ) -> Result<Primitive, Diagnostic> {
        use Primitive as P;
        use Type as T;
        let parameter = T::Parameter(0);
        let list = self.list(parameter, item.span)?;
        let (primitive, count, params, result) = match item.name.as_str() {
            "text_len" => (P::TextLen, 0, vec![T::Text], T::Int),
            "text_byte" => (P::TextByte, 0, vec![T::Text, T::Int], T::Int),
            "text_concat" => (P::TextConcat, 0, vec![T::Text, T::Text], T::Text),
            "text_equal" => (P::TextEqual, 0, vec![T::Text, T::Text], T::Bool),
            "bytes_new" => (P::BytesNew, 0, vec![], T::Bytes),
            "bytes_len" => (P::BytesLen, 0, vec![T::Bytes], T::Int),
            "bytes_push" => (P::BytesPush, 0, vec![T::Bytes, T::Int], T::Unit),
            "bytes_utf8" => (P::BytesUtf8, 0, vec![T::Bytes], T::Bool),
            "bytes_text_copy" => (P::BytesTextCopy, 0, vec![T::Bytes], T::Text),
            "list_new" => (P::ListNew, 1, vec![], list),
            "list_len" => (P::ListLen, 1, vec![list], T::Int),
            "list_get" => (P::ListGet, 1, vec![list, T::Int], parameter),
            "list_push" => (P::ListPush, 1, vec![list, parameter], T::Unit),
            "list_set" => (P::ListSet, 1, vec![list, T::Int, parameter], T::Unit),
            "open" => (P::Open, 0, vec![T::Text], T::Int),
            "create" => (P::Create, 0, vec![T::Text], T::Int),
            "read" => (P::Read, 0, vec![T::Int, T::Bytes, T::Int], T::Int),
            "write" => (P::Write, 0, vec![T::Int, T::Text, T::Int], T::Int),
            "close" => (P::Close, 0, vec![T::Int], T::Int),
            _ => {
                return Err(Diagnostic::new(
                    item.span,
                    "unknown private runtime intrinsic",
                ));
            }
        };
        if item.parameters.len() != count || sig.params != params || sig.result != result {
            return Err(Diagnostic::new(
                item.span,
                "intrinsic declaration does not match its runtime signature",
            ));
        }
        if !item.body.is_empty() || !item.requires.is_empty() || !item.ensures.is_empty() {
            return Err(Diagnostic::new(
                item.span,
                "intrinsic declarations have no source body or contracts; put policy in source wrappers",
            ));
        }
        Ok(primitive)
    }

    fn function(
        &mut self,
        id: usize,
        arguments: Vec<Type>,
        emit: bool,
    ) -> Result<c::Function, Diagnostic> {
        let mut sig = self.signatures[id].clone();
        let item = self.files[sig.file].syntax.functions[sig.function].clone();
        let primitive = if item.intrinsic {
            Some(self.intrinsic(&item, &sig)?)
        } else {
            None
        };
        let package = self.files[sig.file].package.clone();
        sig.params = sig
            .params
            .iter()
            .map(|t| self.substitute(*t, &arguments, item.span))
            .collect::<Result<Vec<_>, _>>()?;
        sig.result = self.substitute(sig.result, &arguments, item.span)?;
        let type_params = item
            .parameters
            .iter()
            .cloned()
            .zip(arguments.iter().copied())
            .collect();
        let mut checker = Checker {
            env: self,
            current: sig.clone(),
            type_params,
            emit,
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
        let body = if let Some(primitive) = primitive {
            let args = sig
                .params
                .iter()
                .enumerate()
                .map(|(local, ty)| c::Expr {
                    kind: c::ExprKind::Local(local),
                    ty: *ty,
                    span: item.span,
                })
                .collect();
            c::Block {
                statements: Vec::new(),
                tail: Some(Box::new(c::Expr {
                    kind: c::ExprKind::Primitive(primitive, args),
                    ty: sig.result,
                    span: item.span,
                })),
                falls_through: true,
            }
        } else {
            checker.block(&item.body, Some(sig.result))?.0
        };
        if body.falls_through && sig.result != Type::Unit && body.tail.is_none() {
            return Err(Diagnostic::new(
                item.span,
                "not every normal path returns a value",
            ));
        }
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
        let mut name = qualified(&package, &item.name);
        if !arguments.is_empty() {
            name.push_str(&format!("{arguments:?}"));
        }
        let function = c::Function {
            name,
            params: sig.params,
            result: sig.result,
            locals: checker.locals,
            requires,
            body,
            span: item.span,
        };
        if !ensures.is_empty() {
            crate::proof::prove(&function, &ensures, result_slot)?;
        }
        Ok(function)
    }
}

fn scalar_contract(expr: &ast::Expr) -> Result<(), Diagnostic> {
    match &expr.kind {
        ast::ExprKind::Int(_) | ast::ExprKind::Bool(_) => Ok(()),
        ast::ExprKind::Name(path) if path.len() == 1 => Ok(()),
        ast::ExprKind::Unary(_, value) => scalar_contract(value),
        ast::ExprKind::Binary(_, a, b) => {
            scalar_contract(a)?;
            scalar_contract(b)
        }
        _ => Err(Diagnostic::new(
            expr.span,
            "the seed supports only scalar, call-free contract predicates",
        )),
    }
}

fn refinement_predicate(expr: &ast::Expr) -> Result<(), Diagnostic> {
    match &expr.kind {
        ast::ExprKind::Int(_) | ast::ExprKind::Bool(_) => Ok(()),
        ast::ExprKind::Name(path) if path == &["self"] => Ok(()),
        ast::ExprKind::Unary(_, value) => refinement_predicate(value),
        ast::ExprKind::Binary(_, a, b) => {
            refinement_predicate(a)?;
            refinement_predicate(b)
        }
        _ => Err(Diagnostic::new(
            expr.span,
            "this slice supports only scalar constraint predicates over self, without calls or external state",
        )),
    }
}

#[derive(Clone, Copy)]
struct Binding {
    local: usize,
    mutable: bool,
}

struct Checker<'a, 'b> {
    env: &'a mut Environment<'b>,
    current: Signature,
    type_params: HashMap<String, Type>,
    emit: bool,
    scopes: Vec<HashMap<String, Binding>>,
    locals: Vec<Type>,
}

impl Checker<'_, '_> {
    fn coerce(&self, value: c::Expr, expected: Type) -> Result<c::Expr, Diagnostic> {
        if value.ty == expected {
            return Ok(value);
        }
        if self.env.widens(value.ty, expected) {
            let span = value.span;
            return Ok(c::Expr {
                kind: c::ExprKind::Coerce(Box::new(value)),
                ty: expected,
                span,
            });
        }
        if !expr_falls(&value) {
            return Ok(value);
        }
        Err(Diagnostic::new(
            value.span,
            format!(
                "expected {expected:?}, found {:?}; use discard to ignore a value",
                value.ty
            ),
        ))
    }

    fn resolve(&mut self, reference: &ast::TypeRef) -> Result<Type, Diagnostic> {
        self.env.resolve(
            reference,
            self.current.file,
            self.current.test_only,
            &self.type_params,
        )
    }

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

    fn local(&self, name: &str) -> Option<Binding> {
        self.scopes.iter().rev().find_map(|s| s.get(name).copied())
    }

    fn expect(&mut self, expr: &ast::Expr, ty: Type) -> Result<c::Expr, Diagnostic> {
        self.expr(expr, Some(ty))
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
                    let annotation = annotation.as_ref().map(|t| self.resolve(t)).transpose()?;
                    let value = self.expr(value, annotation)?;
                    let local = self.bind(name, value.ty, *mutable, stmt.span)?;
                    falls_through &= expr_falls(&value);
                    C::Let { local, value }
                }
                A::Assign { name, value } => {
                    let binding = self.local(name).ok_or_else(|| {
                        Diagnostic::new(stmt.span, format!("unknown local `{name}`"))
                    })?;
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
                A::Discard(expr) => {
                    let value = self.expr(expr, None)?;
                    falls_through &= expr_falls(&value);
                    C::Discard(value)
                }
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
        if falls_through && expected.is_some_and(|expected| ty != expected) {
            return Err(Diagnostic::new(
                block.last().map(|s| s.span).unwrap_or_default(),
                format!(
                    "expected {:?} tail, found {ty:?}; use discard to ignore a value",
                    expected.unwrap()
                ),
            ));
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

    fn field(&self, value: c::Expr, name: &str, span: Span) -> Result<c::Expr, Diagnostic> {
        let Type::Data(id) = value.ty else {
            return Err(Diagnostic::new(
                span,
                "field access requires a known record type",
            ));
        };
        let (file, index) = self.env.declarations[self.env.data_instances[id].declaration];
        let source = &self.env.files[file];
        if source.package != self.env.files[self.current.file].package
            && !source.syntax.data[index].public
        {
            return Err(Diagnostic::new(
                span,
                "cannot inspect fields of a private type from another package",
            ));
        }
        let c::DataKind::Record(fields) = &self.env.types[id].kind else {
            return Err(Diagnostic::new(
                span,
                "enum fields are accessed through match bindings",
            ));
        };
        let Some((index, (_, ty))) = fields.iter().enumerate().find(|(_, (n, _))| n == name) else {
            return Err(Diagnostic::new(span, format!("unknown field `{name}`")));
        };
        Ok(c::Expr {
            kind: c::ExprKind::Field(Box::new(value), index),
            ty: *ty,
            span,
        })
    }

    fn expr(&mut self, expr: &ast::Expr, expected: Option<Type>) -> Result<c::Expr, Diagnostic> {
        use ast::ExprKind as A;
        use c::ExprKind as C;
        let (kind, ty) = match &expr.kind {
            A::Int(value) => (C::Int(*value), Type::Int),
            A::Bool(value) => (C::Bool(*value), Type::Bool),
            A::Text(value) => (C::Text(value.clone()), Type::Text),
            A::Name(path) => {
                if let Some(binding) = self.local(&path[0]) {
                    let mut value = c::Expr {
                        kind: C::Local(binding.local),
                        ty: self.locals[binding.local],
                        span: expr.span,
                    };
                    for name in &path[1..] {
                        value = self.field(value, name, expr.span)?;
                    }
                    (value.kind, value.ty)
                } else if path.len() > 1 {
                    self.variant(path, &[], &[], expected, expr.span)?
                } else {
                    return Err(Diagnostic::new(
                        expr.span,
                        format!("unknown local `{}`", path[0]),
                    ));
                }
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
                let mut left = self.expr(left, None)?;
                if self.env.widens(left.ty, Type::Int) {
                    left = self.coerce(left, Type::Int)?;
                }
                if left.ty == Type::Text && matches!(op, Binary::Eq | Binary::Ne) {
                    let right = self.expect(right, Type::Text)?;
                    let equal = c::Expr {
                        kind: C::Primitive(Primitive::TextEqual, vec![left, right]),
                        ty: Type::Bool,
                        span: expr.span,
                    };
                    let result = if *op == Binary::Ne {
                        c::Expr {
                            kind: C::Unary(Unary::Not, Box::new(equal)),
                            ty: Type::Bool,
                            span: expr.span,
                        }
                    } else {
                        equal
                    };
                    if expected.is_some_and(|t| t != Type::Bool) && expr_falls(&result) {
                        return Err(Diagnostic::new(expr.span, "text comparison produces Bool"));
                    }
                    return Ok(result);
                }
                let argument = match op {
                    Binary::And | Binary::Or => Type::Bool,
                    Binary::Eq | Binary::Ne => left.ty,
                    _ => Type::Int,
                };
                if left.ty != argument || !matches!(left.ty, Type::Int | Type::Bool) {
                    return Err(Diagnostic::new(
                        left.span,
                        "invalid operator operand type; generic operations need an explicit capability",
                    ));
                }
                let right = self.expect(right, argument)?;
                let ty = if matches!(
                    op,
                    Binary::Add | Binary::Sub | Binary::Mul | Binary::Div | Binary::Rem
                ) {
                    Type::Int
                } else {
                    Type::Bool
                };
                (C::Binary(*op, Box::new(left), Box::new(right)), ty)
            }
            A::Call { path, types, args } => self.call(path, types, args, expected, expr.span)?,
            A::Record { ty, fields } => self.record(ty, fields, expected, expr.span)?,
            A::Field(value, name) => {
                let value = self.expr(value, None)?;
                let value = self.field(value, name, expr.span)?;
                (value.kind, value.ty)
            }
            A::Match { value, arms } => self.match_expr(value, arms, expected, expr.span)?,
            A::Block(body) => {
                let (body, ty) = self.block(body, expected)?;
                (C::Block(body), ty)
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
        if let Some(expected) = expected {
            self.coerce(checked, expected)
        } else {
            Ok(checked)
        }
    }

    fn arguments(
        &mut self,
        types: &[ast::TypeRef],
        count: usize,
        span: Span,
    ) -> Result<Vec<Option<Type>>, Diagnostic> {
        if types.is_empty() {
            return Ok(vec![None; count]);
        }
        if types.len() != count {
            return Err(Diagnostic::new(
                span,
                format!("expected {count} type arguments"),
            ));
        }
        types.iter().map(|t| self.resolve(t).map(Some)).collect()
    }

    fn complete(&self, args: Vec<Option<Type>>, span: Span) -> Result<Vec<Type>, Diagnostic> {
        args.into_iter().collect::<Option<Vec<_>>>().ok_or_else(|| {
            Diagnostic::new(
                span,
                "cannot infer type arguments; supply an annotation or explicit type arguments",
            )
        })
    }

    fn call(
        &mut self,
        path: &[String],
        types: &[ast::TypeRef],
        arguments: &[ast::Expr],
        expected: Option<Type>,
        span: Span,
    ) -> Result<(c::ExprKind, Type), Diagnostic> {
        if self.local(&path[0]).is_some() {
            return Err(Diagnostic::new(
                span,
                format!("local `{}` is not callable", path[0]),
            ));
        }
        if let Ok(declaration) =
            self.env
                .data_name(path, self.current.file, self.current.test_only, span)
        {
            let (file, index) = self.env.declarations[declaration];
            if matches!(
                self.env.files[file].syntax.data[index].kind,
                ast::DataKind::Refined { .. }
            ) {
                let (name, _) = split_path(path);
                let packages = self
                    .env
                    .packages(path, self.current.file, self.current.test_only);
                if self.env.signatures.iter().any(|s| {
                    let source = &self.env.files[s.file];
                    let item = &source.syntax.functions[s.function];
                    item.name == name
                        && packages.contains(&source.package)
                        && (source.package == self.env.files[self.current.file].package
                            || item.public)
                        && (!s.test_only || self.current.test_only)
                }) {
                    return Err(Diagnostic::new(
                        span,
                        "a constrained constructor and function share this name; resolve the collision explicitly",
                    ));
                }
                return self.refined(declaration, types, arguments, span);
            }
        }
        if path.len() > 1
            && self
                .env
                .data_name(
                    &path[..path.len() - 1],
                    self.current.file,
                    self.current.test_only,
                    span,
                )
                .is_ok()
        {
            return self.variant(path, types, arguments, expected, span);
        }
        let (name, _) = split_path(path);
        let packages = self
            .env
            .packages(path, self.current.file, self.current.test_only);
        let candidates: Vec<_> = self
            .env
            .signatures
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                let source = &self.env.files[s.file];
                let item = &source.syntax.functions[s.function];
                item.name == name
                    && packages.contains(&source.package)
                    && s.params.len() == arguments.len()
                    && (source.package == self.env.files[self.current.file].package || item.public)
                    && (!s.test_only || self.current.test_only)
                    && (types.is_empty() || item.parameters.len() == types.len())
            })
            .map(|(id, s)| (id, s.clone()))
            .collect();
        // A unique known signature supplies context for zero-payload constructors.
        let mut hints = vec![None; arguments.len()];
        if let [(.., sig)] = candidates.as_slice() {
            let count = self.env.files[sig.file].syntax.functions[sig.function]
                .parameters
                .len();
            let supplied = self.arguments(types, count, span)?;
            if let Some(supplied) = supplied.into_iter().collect::<Option<Vec<_>>>() {
                for (hint, ty) in hints.iter_mut().zip(&sig.params) {
                    *hint = Some(self.env.substitute(*ty, &supplied, span)?);
                }
            }
        }
        let args = arguments
            .iter()
            .zip(hints)
            .map(|(a, hint)| self.expr(a, hint))
            .collect::<Result<Vec<_>, _>>()?;
        let mut matches = Vec::new();
        for (id, sig) in candidates {
            let item = &self.env.files[sig.file].syntax.functions[sig.function];
            let mut inferred = self.arguments(types, item.parameters.len(), span)?;
            let mut widened = false;
            let matched = sig.params.iter().zip(&args).all(|(p, a)| {
                if self.env.infer(*p, a.ty, &mut inferred) {
                    return true;
                }
                let target = match p {
                    Type::Parameter(index) => inferred[*index],
                    _ => Some(*p),
                };
                if target.is_some_and(|target| self.env.widens(a.ty, target)) {
                    widened = true;
                    true
                } else {
                    false
                }
            });
            if !matched {
                continue;
            }
            if inferred.iter().any(Option::is_none) {
                if let Some(expected) = expected {
                    self.env.infer(sig.result, expected, &mut inferred);
                }
            }
            if let Some(inferred) = inferred.into_iter().collect::<Option<Vec<_>>>() {
                matches.push((id, sig, inferred, widened));
            }
        }
        if matches.iter().any(|(_, _, _, widened)| !widened) {
            matches.retain(|(_, _, _, widened)| !widened);
        }
        let (id, sig, inferred, _) = match matches.as_slice() {
            [only] => only.clone(),
            [] => {
                return Err(Diagnostic::new(
                    span,
                    format!(
                        "no accessible overload of `{}` matches these arguments; type arguments may be needed",
                        path.join(".")
                    ),
                ));
            }
            _ => {
                return Err(Diagnostic::new(
                    span,
                    format!(
                        "ambiguous overload of `{}`; qualify the call or supply type arguments",
                        path.join(".")
                    ),
                ));
            }
        };
        let args = args
            .into_iter()
            .zip(&sig.params)
            .map(|(value, ty)| {
                let ty = self.env.substitute(*ty, &inferred, span)?;
                self.coerce(value, ty)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let result = self.env.substitute(sig.result, &inferred, span)?;
        let instance = if self.emit {
            self.env.schedule(id, inferred, span)?
        } else {
            0
        };
        Ok((c::ExprKind::Call(instance, args), result))
    }

    fn refined(
        &mut self,
        declaration: usize,
        types: &[ast::TypeRef],
        arguments: &[ast::Expr],
        span: Span,
    ) -> Result<(c::ExprKind, Type), Diagnostic> {
        if !types.is_empty() || arguments.len() != 1 {
            return Err(Diagnostic::new(
                span,
                "a constrained constructor takes one Int value and no type arguments",
            ));
        }
        let target = self.env.data(declaration, Vec::new(), span)?;
        let value = self.expr(&arguments[0], None)?;
        if value.ty == target {
            return Ok((value.kind, target));
        }
        let value = self.coerce(value, Type::Int)?;
        let (file, index) = self.env.declarations[declaration];
        let ast::DataKind::Refined { predicate, .. } =
            self.env.files[file].syntax.data[index].kind.clone()
        else {
            unreachable!()
        };
        let local = self.locals.len();
        self.locals.push(Type::Int);
        self.scopes.push(HashMap::from([(
            "self".into(),
            Binding {
                local,
                mutable: false,
            },
        )]));
        let predicate = self.expect(&predicate, Type::Bool)?;
        self.scopes.pop();
        match crate::proof::classify_refinement(&value, &predicate, local) {
            Some(true) => {
                self.locals.pop();
                return Ok((c::ExprKind::Coerce(Box::new(value)), target));
            }
            Some(false) => {
                self.locals.pop();
                return Err(Diagnostic::new(
                    span,
                    "this constant does not satisfy the type constraint",
                ));
            }
            None => (),
        }
        let (result, error, ok, err, rejected) = self.env.result_language_items(target, span)?;
        let local_value = c::Expr {
            kind: c::ExprKind::Local(local),
            ty: Type::Int,
            span,
        };
        let success = c::Expr {
            kind: c::ExprKind::Coerce(Box::new(local_value)),
            ty: target,
            span,
        };
        let rejection = c::Expr {
            kind: c::ExprKind::Variant {
                variant: rejected,
                fields: Vec::new(),
            },
            ty: error,
            span,
        };
        let arm = |variant, value| c::Block {
            statements: Vec::new(),
            tail: Some(Box::new(c::Expr {
                kind: c::ExprKind::Variant {
                    variant,
                    fields: vec![value],
                },
                ty: result,
                span,
            })),
            falls_through: true,
        };
        let falls_through = expr_falls(&value);
        let block = c::Block {
            statements: vec![c::Stmt {
                kind: c::StmtKind::Let { local, value },
                span,
            }],
            tail: Some(Box::new(c::Expr {
                kind: c::ExprKind::If {
                    condition: Box::new(predicate),
                    then_body: arm(ok, success),
                    else_body: Some(arm(err, rejection)),
                },
                ty: result,
                span,
            })),
            falls_through,
        };
        Ok((c::ExprKind::Block(block), result))
    }

    fn data_arguments(
        &mut self,
        declaration: usize,
        types: &[ast::TypeRef],
        expected: Option<Type>,
        span: Span,
    ) -> Result<Vec<Option<Type>>, Diagnostic> {
        let (file, index) = self.env.declarations[declaration];
        let count = self.env.files[file].syntax.data[index].parameters.len();
        let mut arguments = self.arguments(types, count, span)?;
        if types.is_empty() {
            if let Some(Type::Data(id)) = expected {
                let instance = &self.env.data_instances[id];
                if instance.declaration == declaration {
                    arguments = instance.arguments.iter().copied().map(Some).collect();
                }
            }
        }
        Ok(arguments)
    }

    fn data_template(&mut self, declaration: usize, span: Span) -> Result<usize, Diagnostic> {
        let (file, index) = self.env.declarations[declaration];
        let count = self.env.files[file].syntax.data[index].parameters.len();
        let Type::Data(id) =
            self.env
                .data(declaration, (0..count).map(Type::Parameter).collect(), span)?
        else {
            unreachable!()
        };
        Ok(id)
    }

    fn hint(
        &mut self,
        pattern: Type,
        args: &[Option<Type>],
        span: Span,
    ) -> Result<Option<Type>, Diagnostic> {
        if let Some(arguments) = args.iter().copied().collect::<Option<Vec<_>>>() {
            return self.env.substitute(pattern, &arguments, span).map(Some);
        }
        if self.env.concrete(pattern) {
            Ok(Some(pattern))
        } else {
            Ok(None)
        }
    }

    fn record(
        &mut self,
        reference: &ast::TypeRef,
        initializers: &[(String, ast::Expr)],
        expected: Option<Type>,
        span: Span,
    ) -> Result<(c::ExprKind, Type), Diagnostic> {
        let declaration = self.env.data_name(
            &reference.path,
            self.current.file,
            self.current.test_only,
            span,
        )?;
        let template = self.data_template(declaration, span)?;
        let c::DataKind::Record(fields) = self.env.types[template].kind.clone() else {
            return Err(Diagnostic::new(
                span,
                "record construction requires a record type",
            ));
        };
        let mut arguments = self.data_arguments(declaration, &reference.args, expected, span)?;
        let mut seen = HashSet::new();
        let mut values = Vec::new();
        for (name, value) in initializers {
            let Some((index, (_, pattern))) =
                fields.iter().enumerate().find(|(_, (n, _))| n == name)
            else {
                return Err(Diagnostic::new(
                    value.span,
                    format!("unknown record field `{name}`"),
                ));
            };
            if !seen.insert(index) {
                return Err(Diagnostic::new(
                    value.span,
                    format!("duplicate initializer for `{name}`"),
                ));
            }
            let hint = self.hint(*pattern, &arguments, value.span)?;
            let value = self.expr(value, hint)?;
            if !self.env.infer(*pattern, value.ty, &mut arguments) {
                return Err(Diagnostic::new(
                    value.span,
                    "record field type does not match",
                ));
            }
            values.push((index, value));
        }
        if seen.len() != fields.len() {
            return Err(Diagnostic::new(
                span,
                "record construction must initialize every field",
            ));
        }
        let arguments = self.complete(arguments, span)?;
        let ty = self.env.data(declaration, arguments, span)?;
        Ok((c::ExprKind::Record(values), ty))
    }

    fn variant(
        &mut self,
        path: &[String],
        types: &[ast::TypeRef],
        fields: &[ast::Expr],
        expected: Option<Type>,
        span: Span,
    ) -> Result<(c::ExprKind, Type), Diagnostic> {
        let declaration = self.env.data_name(
            &path[..path.len() - 1],
            self.current.file,
            self.current.test_only,
            span,
        )?;
        let template = self.data_template(declaration, span)?;
        let c::DataKind::Enum(variants) = self.env.types[template].kind.clone() else {
            return Err(Diagnostic::new(
                span,
                "variant construction requires an enum type",
            ));
        };
        let name = path.last().unwrap();
        let Some((variant, (_, patterns))) =
            variants.iter().enumerate().find(|(_, (n, _))| n == name)
        else {
            return Err(Diagnostic::new(
                span,
                format!("unknown enum variant `{name}`"),
            ));
        };
        if patterns.len() != fields.len() {
            return Err(Diagnostic::new(
                span,
                "enum variant payload count does not match",
            ));
        }
        let mut arguments = self.data_arguments(declaration, types, expected, span)?;
        let mut values = Vec::new();
        for (pattern, value) in patterns.iter().zip(fields) {
            let hint = self.hint(*pattern, &arguments, value.span)?;
            let value = self.expr(value, hint)?;
            if !self.env.infer(*pattern, value.ty, &mut arguments) {
                return Err(Diagnostic::new(
                    value.span,
                    "enum payload type does not match",
                ));
            }
            values.push(value);
        }
        let arguments = self.complete(arguments, span)?;
        let ty = self.env.data(declaration, arguments, span)?;
        Ok((
            c::ExprKind::Variant {
                variant,
                fields: values,
            },
            ty,
        ))
    }

    fn match_expr(
        &mut self,
        value: &ast::Expr,
        arms: &[ast::MatchArm],
        expected: Option<Type>,
        span: Span,
    ) -> Result<(c::ExprKind, Type), Diagnostic> {
        let value = self.expr(value, None)?;
        let (declaration, variants) = match value.ty {
            Type::Data(id) => match &self.env.types[id].kind {
                c::DataKind::Enum(variants) => (
                    Some(self.env.data_instances[id].declaration),
                    variants.clone(),
                ),
                _ => (None, Vec::new()),
            },
            _ => (None, Vec::new()),
        };
        let mut covered = HashSet::new();
        let mut complete = false;
        let mut result_type = expected;
        let mut result = Vec::new();
        for arm in arms {
            if complete {
                return Err(Diagnostic::new(arm.span, "unreachable match arm"));
            }
            self.scopes.push(HashMap::new());
            let (path, bindings) = match &arm.pattern {
                ast::Pattern::Wildcard => (None, None),
                ast::Pattern::Name(path) => (Some(path), None),
                ast::Pattern::Variant { path, bindings } => (Some(path), Some(bindings)),
            };
            let mut variant = None;
            let mut locals = Vec::new();
            let mut whole = None;
            if let Some(path) = path {
                let name = path.last().unwrap();
                let selected = variants.iter().enumerate().find(|(_, (n, _))| n == name);
                if path.len() > 1 {
                    let target = self.env.data_name(
                        &path[..path.len() - 1],
                        self.current.file,
                        self.current.test_only,
                        arm.span,
                    )?;
                    if declaration != Some(target) {
                        return Err(Diagnostic::new(
                            arm.span,
                            "pattern belongs to a different nominal enum",
                        ));
                    }
                }
                if let Some((index, (_, fields))) = selected {
                    let (file, data) = self.env.declarations[declaration.unwrap()];
                    let source = &self.env.files[file];
                    if source.package != self.env.files[self.current.file].package
                        && !source.syntax.data[data].public
                    {
                        return Err(Diagnostic::new(
                            arm.span,
                            "cannot inspect variants of a private type from another package",
                        ));
                    }
                    if !covered.insert(index) {
                        return Err(Diagnostic::new(
                            arm.span,
                            "unreachable duplicate enum variant",
                        ));
                    }
                    variant = Some(index);
                    if bindings.map_or(0, Vec::len) != fields.len() {
                        return Err(Diagnostic::new(
                            arm.span,
                            "pattern payload count does not match",
                        ));
                    }
                    if let Some(bindings) = bindings {
                        for (binding, ty) in bindings.iter().zip(fields) {
                            locals.push(
                                binding
                                    .as_ref()
                                    .map(|name| self.bind(name, *ty, false, arm.span))
                                    .transpose()?,
                            );
                        }
                    }
                    complete = covered.len() == variants.len();
                } else if path.len() == 1 && bindings.is_none() {
                    whole = Some(self.bind(name, value.ty, false, arm.span)?);
                    complete = true;
                } else {
                    return Err(Diagnostic::new(arm.span, "unknown enum variant in pattern"));
                }
            } else {
                complete = true;
            }
            let (body, ty) = self.block(&arm.body, result_type)?;
            if body.falls_through {
                result_type = Some(ty);
            }
            self.scopes.pop();
            result.push(c::MatchArm {
                variant,
                bindings: locals,
                whole,
                body,
            });
        }
        if !complete {
            return Err(Diagnostic::new(
                span,
                "non-exhaustive match; cover all variants or add an irrefutable arm",
            ));
        }
        Ok((
            c::ExprKind::Match {
                value: Box::new(value),
                arms: result,
            },
            result_type.unwrap_or(Type::Unit),
        ))
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
        c::ExprKind::Block(body) => body.falls_through,
        c::ExprKind::Match { value, arms } => {
            expr_falls(value) && arms.iter().any(|a| a.body.falls_through)
        }
        c::ExprKind::Unary(_, e) | c::ExprKind::Field(e, _) | c::ExprKind::Coerce(e) => {
            expr_falls(e)
        }
        c::ExprKind::Binary(Binary::And | Binary::Or, a, _) => expr_falls(a),
        c::ExprKind::Binary(_, a, b) => expr_falls(a) && expr_falls(b),
        c::ExprKind::Call(_, args)
        | c::ExprKind::Primitive(_, args)
        | c::ExprKind::Variant { fields: args, .. } => args.iter().all(expr_falls),
        c::ExprKind::Record(fields) => fields.iter().all(|(_, e)| expr_falls(e)),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn file(package: &str, source: &str, test_only: bool) -> PackageFile {
        PackageFile {
            package: package.into(),
            trusted_std: false,
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

    #[test]
    fn generic_definitions_have_no_hidden_requirements() {
        rejects("pub fn bad[T](x T) T { x + 1 } fn main() { discard bad(1) }");
        rejects("pub fn bad[T](x T) Bool { x == x }");
        rejects("fn specific(x Int) Int { x } pub fn bad[T](x T) Int { specific(x) }");
        let program = check(&[file("", "pub fn identity[T](x T) T { x } fn unused[T](x T) T { x } fn main() { assert identity(2) == identity[Int](2)\nassert identity(true) }", false)], "", false).unwrap();
        assert_eq!(program.functions.len(), 3);
        assert!(program.exports.is_empty());
        assert!(
            program
                .functions
                .iter()
                .all(|f| !f.name.contains("Parameter") && !f.name.contains("unused"))
        );
    }

    #[test]
    fn records_enums_and_generic_matches() {
        let source = "record Pair[A, B] { first A\nsecond B } enum Maybe[T] { None\nSome(T) } fn first[A, B](p Pair[A, B]) A { p.first } fn unwrap[T](m Maybe[T], fallback T) T { match m { Maybe.Some(x) => x\nMaybe.None => fallback } } fn main() { let p = Pair { second = true\nfirst = 7 }\nassert first(p) == 7\nlet empty Maybe[Int] = Maybe.None\nassert unwrap(empty, 4) == 4\nassert unwrap(Maybe.Some(5), 0) == 5 }";
        let program = check(&[file("", source, false)], "", false).unwrap();
        assert_eq!(program.functions.len(), 3);
        rejects("enum Choice { A\nB } fn f(x Choice) Int { match x { Choice.A => 1 } }");
        rejects("enum Choice { A\nB } fn f(x Choice) Int { match x { _ => 1\nChoice.A => 2 } }");
        rejects("record Pair { x Int\ny Int } fn main() { discard Pair { x = 1 } }");
    }

    #[test]
    fn nominal_data_and_private_test_types() {
        let definitions = file(
            "model",
            "pub record Open { x Int } record Secret { x Int }",
            false,
        );
        let app = file(
            "app",
            "import model.Open\nrecord Other { x Int } fn take(x Open) {} fn main() { take(Other { x = 1 }) }",
            false,
        );
        assert!(check(&[definitions.clone(), app], "app", false).is_err());
        let app = file("app", "import model.Secret\nfn main() {}", false);
        assert!(check(&[definitions, app], "app", false).is_err());
        let prod = file("demo", "fn f(x Hidden) {}", false);
        let tests = file("demo", "record Hidden { x Int }", true);
        assert!(check(&[prod, tests], "demo", true).is_err());
        for (definition, body) in [
            (
                "record Hidden { x Int } pub fn get() Hidden { Hidden { x = 1 } }",
                "discard get().x",
            ),
            (
                "enum Hidden { A } pub fn get() Hidden { Hidden.A }",
                "discard match get() { A => 1 }",
            ),
        ] {
            let model = file("model", definition, false);
            let app = file(
                "app",
                &format!("import model.get\nfn main() {{ {body} }}"),
                false,
            );
            assert!(check(&[model, app], "app", false).is_err());
        }
    }

    #[test]
    fn recursive_value_layouts_are_rejected_but_forward_names_work() {
        rejects("record Cycle { next Cycle }");
        rejects("record A { next B } enum B { End\nMore(A) }");
        rejects("record Grow[T] { next Grow[Grow[T]] }");
        rejects(
            "record Wrap[T] { value T } fn grow[T](x T) { grow(Wrap { value = x }) } fn main() { grow(1) }",
        );
        check(&[file("", "record A { next B } record B { number Int } fn main() { assert (A { next = B { number = 9 } }).next.number == 9 }", false)], "", false).unwrap();
    }

    #[test]
    fn intrinsic_declarations_require_trust_and_exact_abstract_signatures() {
        let source = "intrinsic fn text_len(value Text) Int";
        assert!(check(&[file("std.text", source, false)], "std.text", false).is_err());
        for source in [
            "intrinsic fn text_len(value Int) Int",
            "intrinsic fn list_get[T](values List[T], index Int) Int",
            "intrinsic fn list_push[T](values List[T], value Int)",
            "intrinsic fn arbitrary(value Text) Int",
        ] {
            let mut source = file("std.internal", source, false);
            source.trusted_std = true;
            assert!(check(&[source], "std.internal", false).is_err());
        }
    }

    #[test]
    fn shared_lists_are_invariant_and_primitives_stay_behind_source_calls() {
        let mut library = file(
            "collection",
            r#"
intrinsic fn list_new[T]() List[T]
intrinsic fn list_get[T](values List[T], index Int) T
intrinsic fn list_push[T](values List[T], value T)
pub fn make[T]() List[T] { list_new[T]() }
pub fn first[T](values List[T]) T { list_get(values, 0) }
pub fn append[T](values List[T], value T) { list_push(values, value) }
"#,
            false,
        );
        library.trusted_std = true;
        let app = file(
            "app",
            r#"
import collection.make
import collection.first
import collection.append
fn main() {
    let values List[Text] = make()
    let alias = values
    append(alias, "hello")
    assert first(values) == "hello"
    assert first(values) != "goodbye"
}
"#,
            false,
        );
        let program = check(&[library.clone(), app], "app", false).unwrap();
        assert!(program.lists.contains(&Type::Text));
        assert_eq!(program.functions.len(), 7);
        let invalid = file(
            "app",
            "import collection.make\nimport collection.append\nfn main() { let values List[Text] = make()\nappend[Int](values, 1) }",
            false,
        );
        assert!(check(&[library, invalid], "app", false).is_err());
        rejects("fn bad(values List[Int]) Bool { values == values }");
        rejects("fn bad(value Bytes) Bool { value == value }");
        rejects("fn bad(value Text) Bool ensures result { value == value }");
    }

    fn refined_program(source: &str) -> Result<c::Program, Diagnostic> {
        let mut library = file(
            "std.result",
            "pub enum Result[T, E] { Ok(T)\nErr(E) } pub enum ConstraintError { Rejected }",
            false,
        );
        library.trusted_std = true;
        check(&[library, file("app", source, false)], "app", false)
    }

    #[test]
    fn constrained_constants_widen_without_changing_nominal_inference() {
        let program = refined_program(
            r#"
type Positive = Int where self > 0
fn pick(value Positive) Int { 1 }
fn pick(value Int) Int { 2 }
fn id[T](value T) T { value }
fn widened(value Int) Int { value }
fn main() {
    let positive = Positive(6 / 2)
    assert pick(id(positive)) == 1
    assert widened(positive) == 3
    assert positive + 1 == 4
}
"#,
        )
        .unwrap();
        let selected: Vec<_> = program
            .functions
            .iter()
            .filter(|f| f.name == "app.pick")
            .collect();
        assert_eq!(selected.len(), 1);
        assert!(matches!(selected[0].params[0], Type::Data(_)));
        for source in [
            "type Positive = Int where self > 0\nfn main() { discard Positive(0) }",
            "type Positive = Int where self > 0\nfn main() { let value Positive = 1 }",
            "type Positive = Int where self > 0\nfn bad(values List[Positive]) List[Int] { values }",
            "type Positive = Int where self > 0\nfn Positive(value Int) Int { value } fn main() { discard Positive(1) }",
            "type Bad[T] = Int where self > 0",
            "type Bad = List[Int] where true",
            "type A = Int where self > 0\ntype B = A where self > 1",
            "type Bad = Int where self + 1",
            "type Bad = Int where external > 0",
            "fn external() Bool { true } type Bad = Int where external()",
        ] {
            assert!(refined_program(source).is_err(), "{source}");
        }
    }

    #[test]
    fn unknown_constraints_evaluate_once_and_constant_faults_are_not_proofs() {
        let program = refined_program(
            r#"
import std.result.Result
import std.result.ConstraintError
type Positive = Int where self > 0
pub fn checked(value Int) Result[Positive, ConstraintError] { Positive(value) }
"#,
        )
        .unwrap();
        let c::ExprKind::Block(block) = &program.functions[0].body.tail.as_ref().unwrap().kind
        else {
            panic!("missing checked boundary");
        };
        assert_eq!(block.statements.len(), 1);
        assert!(matches!(
            &block.statements[0].kind,
            c::StmtKind::Let {
                value: c::Expr {
                    kind: c::ExprKind::Local(0),
                    ..
                },
                ..
            }
        ));
        for (predicate, input) in [
            ("self > 0", "9223372036854775807 + 1"),
            ("self + 1 > self", "9223372036854775807"),
            ("self > 0", "1 / 0"),
            ("self > 0", "-9223372036854775808 % -1"),
        ] {
            let source = format!(
                "type Guard = Int where {predicate}\nfn main() {{ discard Guard({input}) }}"
            );
            let program = refined_program(&source).unwrap();
            let c::StmtKind::Discard(value) = &program.functions[0].body.statements[0].kind else {
                panic!("missing discard");
            };
            assert!(matches!(value.kind, c::ExprKind::Block(_)), "{source}");
        }
        let library = file(
            "std.result",
            "pub enum Result[T, E] { Ok(T)\nErr(E) } pub enum ConstraintError { Rejected }",
            false,
        );
        let app = file(
            "app",
            "type Positive = Int where self > 0\nfn f(value Int) { discard Positive(value) }",
            false,
        );
        assert!(check(&[library, app], "app", false).is_err());
    }
}
