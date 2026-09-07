//! Decode the source compiler's private checked-IR stream for native codegen.
//! This boundary checks encoding, references and finite layouts, never source
//! names, overloads, contracts or proof. Schema: loom/artifact/artifact.loom.

use crate::model::{Binary, Primitive, Span, Type, Unary, checked as c};

type Result<T> = std::result::Result<T, String>;

struct Reader<'a> {
    text: &'a str,
    offset: usize,
}

impl Reader<'_> {
    fn integer(&mut self) -> Result<i64> {
        let end = self.text[self.offset..]
            .find('\n')
            .map(|end| self.offset + end)
            .ok_or("truncated checked-IR integer")?;
        let value = self.text[self.offset..end]
            .parse()
            .map_err(|_| "invalid checked-IR integer")?;
        self.offset = end + 1;
        Ok(value)
    }

    fn index(&mut self) -> Result<usize> {
        index(self.integer()?)
    }

    fn string(&mut self) -> Result<String> {
        let len = self.index()?;
        let end = self.offset.checked_add(len).ok_or("text length overflow")?;
        let value = self
            .text
            .get(self.offset..end)
            .ok_or("truncated or invalid UTF-8 field")?
            .to_owned();
        self.offset = end;
        Ok(value)
    }

    fn sequence<T>(&mut self, mut item: impl FnMut(&mut Self) -> Result<T>) -> Result<Vec<T>> {
        let count = self.index()?;
        if count > self.text.len() - self.offset {
            return Err("truncated checked-IR sequence".into());
        }
        (0..count).map(|_| item(self)).collect()
    }

    fn span(&mut self) -> Result<Span> {
        let start = self.index()?;
        let end = self.index()?;
        if start > end {
            return Err("reversed checked-IR span".into());
        }
        Ok(Span { start, end })
    }

    fn expression(&mut self, depth: usize) -> Result<Expr> {
        if depth > 512 {
            return Err("checked-IR nesting exceeds the native bridge limit".into());
        }
        let tag = self.index()?;
        let ty = self.index()?;
        let span = self.span()?;
        let text = self.string()?;
        let index = self.integer()?;
        let falls = match self.integer()? {
            0 => false,
            1 => true,
            _ => return Err("invalid checked-IR fallthrough flag".into()),
        };
        let children = self.sequence(|reader| reader.expression(depth + 1))?;
        let arms = self.sequence(|reader| {
            Ok(Arm {
                variant: optional(reader.integer()?)?,
                bindings: reader.sequence(|reader| optional(reader.integer()?))?,
                whole: optional(reader.integer()?)?,
                body: reader.sequence(|reader| reader.expression(depth + 1))?,
            })
        })?;
        Ok(Expr {
            tag,
            ty,
            span,
            text,
            index,
            falls,
            children,
            arms,
        })
    }
}

fn index(value: i64) -> Result<usize> {
    usize::try_from(value).map_err(|_| "negative checked-IR index".into())
}

fn optional(value: i64) -> Result<Option<usize>> {
    if value == -1 {
        Ok(None)
    } else {
        index(value).map(Some)
    }
}

fn at<T>(values: &[T], index: usize) -> Result<&T> {
    values
        .get(index)
        .ok_or_else(|| "checked-IR reference is out of bounds".into())
}

struct Data {
    tag: usize,
    symbol: i64,
    element: usize,
    params: Vec<usize>,
    fields: Vec<(String, usize)>,
    variants: Vec<(String, Vec<usize>)>,
}

struct Function {
    symbol: i64,
    span: Span,
    params: Vec<usize>,
    result: usize,
    locals: Vec<usize>,
    preconditions: Vec<Expr>,
    body: Expr,
}

struct Expr {
    tag: usize,
    ty: usize,
    span: Span,
    text: String,
    index: i64,
    falls: bool,
    children: Vec<Expr>,
    arms: Vec<Arm>,
}

struct Arm {
    variant: Option<usize>,
    bindings: Vec<Option<usize>>,
    whole: Option<usize>,
    body: Vec<Expr>,
}

/// Consume exactly one checked program, rejecting trailing or malformed input.
pub fn decode(text: &str) -> Result<c::Program> {
    let text = text
        .strip_prefix("loom-checked-1\n")
        .ok_or("expected loom-checked-1 header")?;
    let mut reader = Reader { text, offset: 0 };
    let data = reader.sequence(|reader| {
        let tag = reader.index()?;
        let symbol = reader.integer()?;
        let params = if tag == 12 {
            reader.sequence(Reader::index)?
        } else {
            vec![]
        };
        let element = if tag == 6 || tag == 9 || tag == 12 {
            reader.index()?
        } else {
            0
        };
        let fields = if tag == 7 {
            reader.sequence(|reader| Ok((reader.string()?, reader.index()?)))?
        } else {
            vec![]
        };
        let variants = if tag == 8 {
            reader.sequence(|reader| Ok((reader.string()?, reader.sequence(Reader::index)?)))?
        } else {
            vec![]
        };
        Ok(Data {
            tag,
            symbol,
            element,
            params,
            fields,
            variants,
        })
    })?;
    let functions = reader.sequence(|reader| {
        Ok(Function {
            symbol: reader.integer()?,
            span: reader.span()?,
            params: reader.sequence(Reader::index)?,
            result: reader.index()?,
            locals: reader.sequence(Reader::index)?,
            preconditions: reader.sequence(|reader| reader.expression(0))?,
            body: reader.expression(0)?,
        })
    })?;
    let entry = optional(reader.integer()?)?;
    let tests = reader.sequence(Reader::index)?;
    let exports = reader.sequence(Reader::index)?;
    let (interfaces, witnesses) = if reader.offset == text.len() {
        (vec![], vec![])
    } else {
        if reader.integer()? != 1 {
            return Err("unknown checked-IR feature tag".into());
        }
        let interfaces = reader.sequence(|reader| {
            reader.sequence(|reader| Ok((reader.sequence(Reader::index)?, reader.index()?)))
        })?;
        let witnesses = reader.sequence(|reader| {
            Ok((
                reader.index()?,
                reader.index()?,
                reader.sequence(|reader| optional(reader.integer()?))?,
            ))
        })?;
        (interfaces, witnesses)
    };
    if reader.offset != text.len() {
        return Err("trailing checked-IR input".into());
    }
    let mut program = c::Program {
        types: vec![],
        lists: vec![],
        functions: vec![],
        function_types: vec![],
        interfaces: vec![],
        witnesses: vec![],
        entry,
        tests,
        exports,
    };
    let mut types = Vec::new();
    for (id, data) in data.iter().enumerate() {
        types.push(match data.tag {
            0 => Type::Unit,
            1 => Type::Int,
            2 => Type::Bool,
            3 => Type::Text,
            4 => Type::Bytes,
            10 => Type::Float,
            11 => {
                let interface = index(data.symbol)?;
                at(&interfaces, interface)?;
                Type::Dyn(interface)
            }
            12 => {
                if data.symbol != -1 {
                    return Err("checked function types have no declaration symbol".into());
                }
                let id = program.function_types.len();
                program.function_types.push(c::Signature {
                    params: vec![],
                    result: Type::Unit,
                });
                Type::Function(id)
            }
            5 => Type::Parameter(index(data.symbol)?),
            6 => {
                let id = program.lists.len();
                program.lists.push(Type::Unit);
                Type::List(id)
            }
            7..=9 => {
                let target = program.types.len();
                program.types.push(c::Data {
                    name: format!("source.type.{}.{id}", data.symbol),
                    kind: c::DataKind::Record(vec![]),
                });
                Type::Data(target)
            }
            _ => return Err("unknown checked-IR type tag".into()),
        });
    }
    let map = |id| at(&types, id).copied();
    for methods in interfaces {
        program.interfaces.push(c::Interface {
            methods: methods
                .into_iter()
                .map(|(params, result)| {
                    Ok(c::Signature {
                        params: params.into_iter().map(map).collect::<Result<_>>()?,
                        result: map(result)?,
                    })
                })
                .collect::<Result<_>>()?,
        });
    }
    for (interface, concrete, methods) in witnesses {
        let shape = at(&program.interfaces, interface)?;
        let concrete = map(concrete)?;
        if matches!(concrete, Type::Dyn(_) | Type::Parameter(_) | Type::Unit) {
            return Err("checked witness requires a concrete value type".into());
        }
        if methods.len() != shape.methods.len() {
            return Err("checked witness method count mismatch".into());
        }
        for (target, method) in methods.iter().zip(&shape.methods) {
            if let Some(target) = target {
                let target = at(&functions, *target)?;
                let params = target
                    .params
                    .iter()
                    .map(|ty| map(*ty))
                    .collect::<Result<Vec<_>>>()?;
                if params.first() != Some(&concrete)
                    || params[1..] != method.params
                    || map(target.result)? != method.result
                {
                    return Err("checked witness method signature mismatch".into());
                }
            }
        }
        program.witnesses.push(c::Witness {
            interface,
            concrete,
            methods,
        });
    }
    for (data, mapped) in data.iter().zip(&types) {
        match *mapped {
            Type::List(id) => program.lists[id] = map(data.element)?,
            Type::Function(id) => {
                program.function_types[id] = c::Signature {
                    params: data
                        .params
                        .iter()
                        .map(|ty| map(*ty))
                        .collect::<Result<_>>()?,
                    result: map(data.element)?,
                };
            }
            Type::Data(id) => {
                program.types[id].kind = match data.tag {
                    7 => c::DataKind::Record(
                        data.fields
                            .iter()
                            .map(|(name, ty)| Ok((name.clone(), map(*ty)?)))
                            .collect::<Result<_>>()?,
                    ),
                    8 => c::DataKind::Enum(
                        data.variants
                            .iter()
                            .map(|(name, fields)| {
                                Ok((
                                    name.clone(),
                                    fields.iter().map(|ty| map(*ty)).collect::<Result<_>>()?,
                                ))
                            })
                            .collect::<Result<_>>()?,
                    ),
                    9 => c::DataKind::Refined(map(data.element)?),
                    _ => unreachable!(),
                }
            }
            _ => {}
        }
    }
    let mut state = vec![0; program.types.len()];
    for id in 0..program.types.len() {
        layout(&program, Type::Data(id), &mut state)?;
    }
    for (id, source) in functions.iter().enumerate() {
        let converter = Converter {
            program: &program,
            types: &types,
            functions: &functions,
            source,
        };
        let params = source
            .params
            .iter()
            .map(|ty| map(*ty))
            .collect::<Result<Vec<_>>>()?;
        let locals = source
            .locals
            .iter()
            .map(|ty| map(*ty))
            .collect::<Result<Vec<_>>>()?;
        if !locals.starts_with(&params) {
            return Err("checked parameters must occupy leading local slots".into());
        }
        let requires = source
            .preconditions
            .iter()
            .map(|expr| {
                if map(expr.ty)? != Type::Bool {
                    return Err("precondition must be a Bool value".into());
                }
                converter.expr(expr)
            })
            .collect::<Result<_>>()?;
        let body = converter.block(&source.body)?;
        program.functions.push(c::Function {
            name: format!("source.fn.{}.{id}", source.symbol),
            params,
            result: map(source.result)?,
            locals,
            requires,
            body,
            span: source.span,
        });
    }
    for root in program
        .entry
        .iter()
        .chain(&program.tests)
        .chain(&program.exports)
    {
        at(&program.functions, *root)?;
    }
    Ok(program)
}

fn layout(program: &c::Program, ty: Type, state: &mut [u8]) -> Result<()> {
    let Type::Data(id) = ty else { return Ok(()) };
    if state[id] == 2 {
        return Ok(());
    }
    if state[id] == 1 {
        return Err("recursive by-value checked-IR layout".into());
    }
    state[id] = 1;
    match &program.types[id].kind {
        c::DataKind::Record(fields) => {
            for (_, ty) in fields {
                layout(program, *ty, state)?;
            }
        }
        c::DataKind::Enum(variants) => {
            for (_, fields) in variants {
                for ty in fields {
                    layout(program, *ty, state)?;
                }
            }
        }
        c::DataKind::Refined(base) => {
            if *base != Type::Int && *base != Type::Float {
                return Err("native refined layout must have an Int or Float base".into());
            }
        }
    }
    state[id] = 2;
    Ok(())
}

struct Converter<'a> {
    program: &'a c::Program,
    types: &'a [Type],
    functions: &'a [Function],
    source: &'a Function,
}

impl Converter<'_> {
    fn ty(&self, id: usize) -> Result<Type> {
        at(self.types, id).copied()
    }

    fn local(&self, id: i64) -> Result<usize> {
        let id = index(id)?;
        at(&self.source.locals, id)?;
        Ok(id)
    }

    fn child<'a>(&self, node: &'a Expr, id: usize, count: usize) -> Result<&'a Expr> {
        if node.children.len() != count {
            return Err("wrong checked-IR node arity".into());
        }
        at(&node.children, id)
    }

    fn record(&self, ty: Type) -> Result<&[(String, Type)]> {
        if let Type::Data(id) = ty
            && let c::DataKind::Record(fields) = &self.program.types[id].kind
        {
            return Ok(fields);
        }
        Err("checked field requires a record layout".into())
    }

    fn variant(&self, ty: Type, variant: usize) -> Result<&[Type]> {
        if let Type::Data(id) = ty
            && let c::DataKind::Enum(variants) = &self.program.types[id].kind
        {
            return Ok(&at(variants, variant)?.1);
        }
        Err("checked variant requires an enum layout".into())
    }

    fn block(&self, node: &Expr) -> Result<c::Block> {
        if node.tag != 11 {
            return Err("expected a checked block".into());
        }
        let mut statements = Vec::new();
        let mut tail = None;
        for (index, child) in node.children.iter().enumerate() {
            if index + 1 == node.children.len() && !(12..=17).contains(&child.tag) {
                tail = Some(Box::new(self.expr(child)?));
            } else {
                statements.push(self.statement(child)?);
            }
        }
        Ok(c::Block {
            statements,
            tail,
            falls_through: node.falls,
        })
    }

    fn statement(&self, node: &Expr) -> Result<c::Stmt> {
        use c::StmtKind as S;
        let kind = match node.tag {
            12 | 13 => {
                let local = self.local(node.index)?;
                let value = self.expr(self.child(node, 0, 1)?)?;
                if value.ty != self.ty(self.source.locals[local])? {
                    return Err("checked local storage type mismatch".into());
                }
                if node.tag == 12 {
                    S::Let { local, value }
                } else {
                    S::Assign { local, value }
                }
            }
            14 => {
                if node.children.len() > 1 {
                    return Err("return accepts at most one checked value".into());
                }
                S::Return(
                    node.children
                        .first()
                        .map(|value| self.expr(value))
                        .transpose()?,
                )
            }
            15 => S::Assert(self.expr(self.child(node, 0, 1)?)?),
            16 => S::Discard(self.expr(self.child(node, 0, 1)?)?),
            17 => S::While {
                condition: self.expr(self.child(node, 0, 2)?)?,
                body: self.block(self.child(node, 1, 2)?)?,
            },
            _ => S::Expr(self.expr(node)?),
        };
        Ok(c::Stmt {
            kind,
            span: node.span,
        })
    }

    fn expr(&self, node: &Expr) -> Result<c::Expr> {
        use c::ExprKind as E;
        let ty = self.ty(node.ty)?;
        if node.tag != 19 && !node.arms.is_empty() {
            return Err("only match nodes have checked arms".into());
        }
        let kind = match node.tag {
            0 => E::Int(
                node.text
                    .parse()
                    .map_err(|_| "invalid checked Int literal")?,
            ),
            21 => E::Float(
                node.text
                    .parse()
                    .map_err(|_| "invalid checked Float literal")?,
            ),
            1 => E::Bool(match node.text.as_str() {
                "true" => true,
                "false" => false,
                _ => return Err("invalid checked Bool literal".into()),
            }),
            2 => E::Text(node.text.clone()),
            3 => E::Local(self.local(node.index)?),
            4 => {
                let operation = match node.text.as_str() {
                    "-" => Unary::Neg,
                    "!" => Unary::Not,
                    "~" => Unary::BitNot,
                    _ => return Err("unknown checked unary operation".into()),
                };
                let value = self.expr(self.child(node, 0, 1)?)?;
                if operation == Unary::BitNot && (ty != Type::Int || value.ty != Type::Int) {
                    return Err("checked bitwise operations require Int operands and result".into());
                }
                E::Unary(operation, Box::new(value))
            }
            5 => {
                let operation = binary(&node.text)?;
                let left = self.expr(self.child(node, 0, 2)?)?;
                let right = self.expr(self.child(node, 1, 2)?)?;
                if matches!(
                    operation,
                    Binary::BitAnd | Binary::BitOr | Binary::BitXor | Binary::Shl | Binary::Shr
                ) && (ty != Type::Int || left.ty != Type::Int || right.ty != Type::Int)
                {
                    return Err("checked bitwise operations require Int operands and result".into());
                }
                E::Binary(operation, Box::new(left), Box::new(right))
            }
            6 => {
                let id = index(node.index)?;
                let function = at(self.functions, id)?;
                if function.params.len() != node.children.len() {
                    return Err("checked call arity mismatch".into());
                }
                E::Call(
                    id,
                    node.children
                        .iter()
                        .map(|child| self.expr(child))
                        .collect::<Result<_>>()?,
                )
            }
            7 => {
                let operation = primitive(&node.text)?;
                if node.children.len() != primitive_arity(operation) {
                    return Err("checked runtime operation arity mismatch".into());
                }
                let arguments = node
                    .children
                    .iter()
                    .map(|child| self.expr(child))
                    .collect::<Result<Vec<_>>>()?;
                if matches!(
                    operation,
                    Primitive::ProcessCapture | Primitive::ProcessCaptureConfigured
                ) {
                    let text_list =
                        |ty| matches!(ty, Type::List(id) if self.program.lists[id] == Type::Text);
                    let buffers = if operation == Primitive::ProcessCapture {
                        1
                    } else {
                        4
                    };
                    if ty != Type::Int
                        || !text_list(arguments[0].ty)
                        || arguments[buffers].ty != Type::Bytes
                        || arguments[buffers + 1].ty != Type::Bytes
                        || (operation == Primitive::ProcessCaptureConfigured
                            && (arguments[1].ty != Type::Text
                                || arguments[2].ty != Type::Int
                                || !text_list(arguments[3].ty)))
                    {
                        return Err("checked process capture type mismatch".into());
                    }
                }
                let signature: Option<(&[Type], Type)> = match operation {
                    Primitive::BytesGet => Some((&[Type::Bytes, Type::Int], Type::Int)),
                    Primitive::BytesSet => Some((&[Type::Bytes, Type::Int, Type::Int], Type::Unit)),
                    Primitive::WriteBytes => {
                        Some((&[Type::Int, Type::Bytes, Type::Int], Type::Int))
                    }
                    Primitive::DirectoryCreate
                    | Primitive::FileRemove
                    | Primitive::DirectoryRemove
                    | Primitive::PathEntryKind => Some((&[Type::Text], Type::Int)),
                    Primitive::PathRename => Some((&[Type::Text, Type::Text], Type::Int)),
                    Primitive::EnvGet => Some((&[Type::Text, Type::Bytes], Type::Int)),
                    _ => None,
                };
                if let Some((params, result)) = signature {
                    if ty != result
                        || !arguments
                            .iter()
                            .map(|arg| arg.ty)
                            .eq(params.iter().copied())
                    {
                        return Err("checked runtime operation type mismatch".into());
                    }
                }
                E::Primitive(operation, arguments)
            }
            8 => {
                let fields = self.record(ty)?;
                if fields.len() != node.children.len() {
                    return Err("checked record initializer count mismatch".into());
                }
                let mut seen = vec![false; fields.len()];
                let mut values = Vec::new();
                for field in &node.children {
                    if field.tag != 9 {
                        return Err("record children must be indexed field wrappers".into());
                    }
                    let id = index(field.index)?;
                    at(fields, id)?;
                    if seen[id] {
                        return Err("duplicate checked record field".into());
                    }
                    seen[id] = true;
                    values.push((id, self.expr(self.child(field, 0, 1)?)?));
                }
                E::Record(values)
            }
            9 => {
                let value = self.expr(self.child(node, 0, 1)?)?;
                let id = index(node.index)?;
                at(self.record(value.ty)?, id)?;
                E::Field(Box::new(value), id)
            }
            10 => {
                let variant = index(node.index)?;
                if self.variant(ty, variant)?.len() != node.children.len() {
                    return Err("checked variant payload count mismatch".into());
                }
                E::Variant {
                    variant,
                    fields: node
                        .children
                        .iter()
                        .map(|child| self.expr(child))
                        .collect::<Result<_>>()?,
                }
            }
            11 => E::Block(self.block(node)?),
            18 => {
                if !(2..=3).contains(&node.children.len()) {
                    return Err("checked if needs two or three children".into());
                }
                E::If {
                    condition: Box::new(self.expr(&node.children[0])?),
                    then_body: self.block(&node.children[1])?,
                    else_body: node
                        .children
                        .get(2)
                        .map(|body| self.block(body))
                        .transpose()?,
                }
            }
            19 => {
                let value = self.expr(self.child(node, 0, 1)?)?;
                let mut arms = Vec::new();
                for arm in &node.arms {
                    if let Some(variant) = arm.variant
                        && self.variant(value.ty, variant)?.len() != arm.bindings.len()
                    {
                        return Err("checked match binding count mismatch".into());
                    }
                    for local in arm.bindings.iter().chain([&arm.whole]).flatten() {
                        at(&self.source.locals, *local)?;
                    }
                    if arm.body.len() != 1 {
                        return Err("checked match arm needs one block".into());
                    }
                    arms.push(c::MatchArm {
                        variant: arm.variant,
                        bindings: arm.bindings.clone(),
                        whole: arm.whole,
                        body: self.block(&arm.body[0])?,
                    });
                }
                E::Match {
                    value: Box::new(value),
                    arms,
                }
            }
            20 => E::Coerce(Box::new(self.expr(self.child(node, 0, 1)?)?)),
            22 => {
                let witness = index(node.index)?;
                let table = at(&self.program.witnesses, witness)?;
                let value = self.expr(self.child(node, 0, 1)?)?;
                if ty != Type::Dyn(table.interface) || value.ty != table.concrete {
                    return Err("checked dyn construction type mismatch".into());
                }
                E::DynBox {
                    witness,
                    value: Box::new(value),
                }
            }
            23 => {
                let receiver = self.expr(at(&node.children, 0)?)?;
                let Type::Dyn(interface) = receiver.ty else {
                    return Err("checked dyn call requires an erased receiver".into());
                };
                let slot = index(node.index)?;
                let method = at(&at(&self.program.interfaces, interface)?.methods, slot)?;
                if method.params.len() + 1 != node.children.len() || ty != method.result {
                    return Err("checked dyn call signature mismatch".into());
                }
                let arguments = node.children[1..]
                    .iter()
                    .map(|value| self.expr(value))
                    .collect::<Result<Vec<_>>>()?;
                if arguments
                    .iter()
                    .map(|value| value.ty)
                    .ne(method.params.iter().copied())
                {
                    return Err("checked dyn call argument type mismatch".into());
                }
                E::DynCall {
                    receiver: Box::new(receiver),
                    slot,
                    arguments,
                }
            }
            24 => {
                let Type::Function(signature) = ty else {
                    return Err("checked function reference requires a function type".into());
                };
                if !node.children.is_empty() {
                    return Err("checked named function references cannot capture values".into());
                }
                let target = index(node.index)?;
                let function = at(self.functions, target)?;
                let signature = at(&self.program.function_types, signature)?;
                let params = function
                    .params
                    .iter()
                    .map(|ty| self.ty(*ty))
                    .collect::<Result<Vec<_>>>()?;
                if params != signature.params || self.ty(function.result)? != signature.result {
                    return Err("checked function reference signature mismatch".into());
                }
                E::FunctionRef(target)
            }
            25 => {
                if node.index != -1 {
                    return Err("checked indirect call has no direct function ID".into());
                }
                let callee = self.expr(at(&node.children, 0)?)?;
                let Type::Function(signature) = callee.ty else {
                    return Err("checked indirect call requires a function value".into());
                };
                let signature = at(&self.program.function_types, signature)?;
                let arguments = node.children[1..]
                    .iter()
                    .map(|value| self.expr(value))
                    .collect::<Result<Vec<_>>>()?;
                if ty != signature.result
                    || arguments
                        .iter()
                        .map(|value| value.ty)
                        .ne(signature.params.iter().copied())
                {
                    return Err("checked indirect call signature mismatch".into());
                }
                E::IndirectCall {
                    callee: Box::new(callee),
                    arguments,
                }
            }
            _ => return Err("unknown or misplaced checked expression tag".into()),
        };
        Ok(c::Expr {
            kind,
            ty,
            span: node.span,
        })
    }
}

fn binary(value: &str) -> Result<Binary> {
    use Binary as B;
    Ok(match value {
        "+" => B::Add,
        "-" => B::Sub,
        "*" => B::Mul,
        "/" => B::Div,
        "%" => B::Rem,
        "==" => B::Eq,
        "!=" => B::Ne,
        "<" => B::Lt,
        "<=" => B::Le,
        ">" => B::Gt,
        ">=" => B::Ge,
        "&&" => B::And,
        "||" => B::Or,
        "&" => B::BitAnd,
        "|" => B::BitOr,
        "^" => B::BitXor,
        "<<" => B::Shl,
        ">>" => B::Shr,
        _ => return Err("unknown checked binary operation".into()),
    })
}

fn primitive(value: &str) -> Result<Primitive> {
    use Primitive as P;
    Ok(match value {
        "float_from_int" => P::FloatFromInt,
        "float_to_int" => P::FloatToInt,
        "float_parse" => P::FloatParse,
        "float_format" => P::FloatFormat,
        "text_len" => P::TextLen,
        "text_byte" => P::TextByte,
        "text_concat" => P::TextConcat,
        "text_equal" => P::TextEqual,
        "text_slice" => P::TextSlice,
        "unicode_alphabetic" => P::UnicodeAlphabetic,
        "unicode_alphanumeric" => P::UnicodeAlphanumeric,
        "unicode_whitespace" => P::UnicodeWhitespace,
        "arg_count" => P::ArgCount,
        "arg_text" => P::ArgText,
        "exit" => P::Exit,
        "process_run" => P::ProcessRun,
        "process_run_input" => P::ProcessRunInput,
        "process_capture" => P::ProcessCapture,
        "process_capture_configured" => P::ProcessCaptureConfigured,
        "env_get" => P::EnvGet,
        "bytes_new" => P::BytesNew,
        "bytes_len" => P::BytesLen,
        "bytes_get" => P::BytesGet,
        "bytes_push" => P::BytesPush,
        "bytes_set" => P::BytesSet,
        "bytes_utf8" => P::BytesUtf8,
        "bytes_text_copy" => P::BytesTextCopy,
        "list_new" => P::ListNew,
        "list_len" => P::ListLen,
        "list_get" => P::ListGet,
        "list_push" => P::ListPush,
        "list_set" => P::ListSet,
        "open" => P::Open,
        "create" => P::Create,
        "read" => P::Read,
        "write" => P::Write,
        "write_bytes" => P::WriteBytes,
        "close" => P::Close,
        "directory_read" => P::DirectoryRead,
        "path_kind" => P::PathKind,
        "path_canonical" => P::PathCanonical,
        "directory_create" => P::DirectoryCreate,
        "path_rename" => P::PathRename,
        "file_remove" => P::FileRemove,
        "directory_remove" => P::DirectoryRemove,
        "path_entry_kind" => P::PathEntryKind,
        _ => return Err("unknown private checked runtime operation".into()),
    })
}

fn primitive_arity(operation: Primitive) -> usize {
    use Primitive as P;
    match operation {
        P::ArgCount | P::BytesNew | P::ListNew => 0,
        P::FloatFromInt
        | P::FloatToInt
        | P::FloatParse
        | P::FloatFormat
        | P::TextLen
        | P::UnicodeAlphabetic
        | P::UnicodeAlphanumeric
        | P::UnicodeWhitespace
        | P::ArgText
        | P::Exit
        | P::ProcessRun
        | P::BytesLen
        | P::BytesUtf8
        | P::BytesTextCopy
        | P::ListLen
        | P::Open
        | P::Create
        | P::Close
        | P::PathKind
        | P::DirectoryCreate
        | P::FileRemove
        | P::DirectoryRemove
        | P::PathEntryKind => 1,
        P::TextByte
        | P::TextConcat
        | P::TextEqual
        | P::ProcessRunInput
        | P::BytesGet
        | P::BytesPush
        | P::ListGet
        | P::ListPush
        | P::DirectoryRead
        | P::PathCanonical
        | P::EnvGet
        | P::PathRename => 2,
        P::TextSlice
        | P::BytesSet
        | P::ListSet
        | P::Read
        | P::Write
        | P::WriteBytes
        | P::ProcessCapture => 3,
        P::ProcessCaptureConfigured => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(tag: usize, ty: usize, text: &str, index: i64, children: &[String]) -> String {
        format!(
            "{tag}\n{ty}\n0\n0\n{}\n{text}{index}\n1\n{}\n{}0\n",
            text.len(),
            children.len(),
            children.concat()
        )
    }

    #[test]
    fn filesystem_wire_operations_require_text_paths_and_int_status() {
        let stream = |operation: &str, arguments: &[String], result| {
            let call = node(7, result, operation, -1, arguments);
            let body = node(11, result, "", -1, &[call]);
            format!(
                "loom-checked-1\n3\n0\n-1\n1\n-1\n3\n-1\n1\n0\n0\n0\n0\n{result}\n0\n0\n{body}-1\n0\n1\n0\n"
            )
        };
        for name in [
            "directory_create",
            "path_rename",
            "file_remove",
            "directory_remove",
            "path_entry_kind",
        ] {
            let operation = primitive(name).unwrap();
            let arguments = vec![node(2, 2, "path", -1, &[]); primitive_arity(operation)];
            let program = decode(&stream(name, &arguments, 1)).unwrap();
            assert!(matches!(
                program.functions[0].body.tail.as_ref().unwrap().kind,
                c::ExprKind::Primitive(actual, _) if actual == operation
            ));
            assert!(
                decode(&stream(name, &arguments, 2))
                    .unwrap_err()
                    .contains("runtime operation type mismatch")
            );
            for index in 0..arguments.len() {
                let mut invalid = arguments.clone();
                invalid[index] = node(0, 1, "0", -1, &[]);
                assert!(
                    decode(&stream(name, &invalid, 1))
                        .unwrap_err()
                        .contains("runtime operation type mismatch")
                );
            }
            assert!(
                decode(&stream(name, &[], 1))
                    .unwrap_err()
                    .contains("arity mismatch")
            );
        }
    }

    #[test]
    fn process_operations_have_exact_wire_types() {
        let stream = |name: &str, params: &[usize], result| {
            let arguments = params
                .iter()
                .enumerate()
                .map(|(index, ty)| node(3, *ty, "", index as i64, &[]))
                .collect::<Vec<_>>();
            let call = node(7, result, name, -1, &arguments);
            let body = node(11, result, "", -1, &[call]);
            let params = format!(
                "{}\n{}",
                params.len(),
                params
                    .iter()
                    .map(|ty| format!("{ty}\n"))
                    .collect::<String>()
            );
            format!(
                "loom-checked-1\n6\n0\n-1\n1\n-1\n3\n-1\n4\n-1\n6\n-1\n2\n6\n-1\n1\n1\n0\n0\n0\n{params}{result}\n{params}0\n{body}-1\n0\n1\n0\n"
            )
        };
        for (name, params) in [
            ("process_capture", &[4, 3, 3][..]),
            ("process_capture_configured", &[4, 2, 1, 4, 3, 3][..]),
            ("env_get", &[2, 3][..]),
        ] {
            let operation = primitive(name).unwrap();
            let program = decode(&stream(name, params, 1)).unwrap();
            assert!(matches!(
                program.functions[0].body.tail.as_ref().unwrap().kind,
                c::ExprKind::Primitive(actual, _) if actual == operation
            ));
            for index in 0..params.len() {
                let mut invalid = params.to_vec();
                invalid[index] = match params[index] {
                    4 => 5,
                    3 => 2,
                    _ => 3,
                };
                assert!(
                    decode(&stream(name, &invalid, 1))
                        .unwrap_err()
                        .contains("type mismatch")
                );
            }
            assert!(
                decode(&stream(name, params, 0))
                    .unwrap_err()
                    .contains("type mismatch")
            );
            assert!(
                decode(&stream(name, &params[..params.len() - 1], 1))
                    .unwrap_err()
                    .contains("arity mismatch")
            );
        }
    }

    #[test]
    fn bitwise_wire_operations_require_int_operands_and_result() {
        let stream = |value: String, result| {
            let body = node(11, result, "", -1, &[value]);
            format!(
                "loom-checked-1\n4\n0\n-1\n1\n-1\n2\n-1\n10\n-1\n1\n0\n0\n0\n0\n{result}\n0\n0\n{body}-1\n0\n1\n0\n"
            )
        };
        let integer = node(0, 1, "7", -1, &[]);
        for operation in ["~", "&", "|", "^", "<<", ">>"] {
            let unary = operation == "~";
            let tag = if unary { 4 } else { 5 };
            let operands = if unary {
                vec![integer.clone()]
            } else {
                vec![integer.clone(), integer.clone()]
            };
            let program = decode(&stream(node(tag, 1, operation, -1, &operands), 1)).unwrap();
            let actual = &program.functions[0].body.tail.as_ref().unwrap().kind;
            if unary {
                assert!(matches!(actual, c::ExprKind::Unary(Unary::BitNot, _)));
            } else {
                let c::ExprKind::Binary(actual, _, _) = actual else {
                    panic!("expected a binary operation")
                };
                assert_eq!(*actual, binary(operation).unwrap());
            }
            assert!(
                decode(&stream(node(tag, 2, operation, -1, &operands), 2))
                    .unwrap_err()
                    .contains("require Int")
            );
            for replacement in [node(1, 2, "true", -1, &[]), node(21, 3, "1.0", -1, &[])] {
                for index in 0..operands.len() {
                    let mut invalid = operands.clone();
                    invalid[index] = replacement.clone();
                    assert!(
                        decode(&stream(node(tag, 1, operation, -1, &invalid), 1))
                            .unwrap_err()
                            .contains("require Int")
                    );
                }
            }
        }
    }

    #[test]
    fn function_references_and_indirect_calls_validate_exact_signatures() {
        let identity = node(11, 1, "", -1, &[node(3, 1, "", 0, &[])]);
        let reference = node(24, 2, "", 0, &[]);
        let argument = node(0, 1, "7", -1, &[]);
        let stream = |reference: String, argument: String, result| {
            let call = node(25, result, "", -1, &[reference, argument]);
            let main = node(11, 0, "", -1, &[node(16, 0, "", -1, &[call])]);
            format!(
                "loom-checked-1\n3\n0\n-1\n1\n-1\n12\n-1\n1\n1\n1\n2\n0\n0\n0\n1\n1\n1\n1\n1\n0\n{identity}1\n0\n0\n0\n0\n0\n0\n{main}1\n0\n0\n"
            )
        };
        let program = decode(&stream(reference.clone(), argument.clone(), 1)).unwrap();
        assert_eq!(program.function_types[0].params, [Type::Int]);
        assert_eq!(program.function_types[0].result, Type::Int);
        let c::StmtKind::Discard(value) = &program.functions[1].body.statements[0].kind else {
            panic!()
        };
        let c::ExprKind::IndirectCall { callee, .. } = &value.kind else {
            panic!()
        };
        assert_eq!(callee.ty, Type::Function(0));
        assert!(matches!(callee.kind, c::ExprKind::FunctionRef(0)));
        assert!(
            decode(&stream(node(24, 2, "", 1, &[]), argument.clone(), 1))
                .unwrap_err()
                .contains("reference signature")
        );
        assert!(
            decode(&stream(reference.clone(), node(24, 2, "", 0, &[]), 1))
                .unwrap_err()
                .contains("call signature")
        );
        assert!(
            decode(&stream(reference, argument.clone(), 0))
                .unwrap_err()
                .contains("call signature")
        );
        assert!(
            decode(&stream(
                node(24, 2, "", 0, &[argument.clone()]),
                argument,
                1
            ))
            .unwrap_err()
            .contains("cannot capture")
        );
    }

    #[test]
    fn private_stream_is_counted_and_exact() {
        let empty = "loom-checked-1\n0\n0\n-1\n0\n0\n";
        assert!(decode(empty).unwrap().functions.is_empty());
        assert!(decode(&format!("{empty}extra")).is_err());
        assert!(decode("loom source is not a checked artifact").is_err());
        assert!(decode("loom-checked-1\n100\n").is_err());
        let mut reader = Reader {
            text: "4\né\n\0",
            offset: 0,
        };
        assert_eq!(reader.string().unwrap(), "é\n\0");
    }

    #[test]
    fn invalid_references_and_by_value_cycles_reject_before_codegen() {
        let cycle = "loom-checked-1\n1\n7\n1\n1\n4\nnext0\n0\n-1\n0\n0\n";
        assert!(decode(cycle).unwrap_err().contains("recursive by-value"));
        let invalid = "loom-checked-1\n1\n9\n0\n8\n0\n-1\n0\n0\n";
        assert!(decode(invalid).unwrap_err().contains("out of bounds"));
    }

    #[test]
    fn float_wire_literals_preserve_ieee_edges() {
        for (literal, expected) in [
            ("1.25", 1.25_f64),
            ("-0.0", -0.0),
            ("1e9999", f64::INFINITY),
            ("-1e-9999", -0.0),
        ] {
            // One Float type and a public function whose block returns tag 21.
            let stream = format!(
                "loom-checked-1\n1\n10\n-1\n1\n0\n0\n0\n0\n0\n0\n0\n\
                 11\n0\n0\n0\n0\n-1\n1\n1\n\
                 21\n0\n0\n0\n{}\n{literal}-1\n1\n0\n0\n\
                 0\n-1\n0\n1\n0\n",
                literal.len()
            );
            let program = decode(&stream).unwrap();
            assert_eq!(program.functions[0].result, Type::Float);
            let c::ExprKind::Float(value) = program.functions[0].body.tail.as_ref().unwrap().kind
            else {
                panic!("expected a Float literal");
            };
            assert_eq!(value.to_bits(), expected.to_bits());
            assert!(decode(&stream.replace(literal, &"x".repeat(literal.len()))).is_err());
        }
        for name in [
            "float_from_int",
            "float_to_int",
            "float_parse",
            "float_format",
        ] {
            assert_eq!(primitive_arity(primitive(name).unwrap()), 1);
        }
    }

    #[test]
    fn dynamic_tail_decodes_nominal_tables_and_checks_signatures() {
        let identity = node(11, 1, "", -1, &[node(3, 1, "", 0, &[])]);
        let boxed = node(22, 2, "", 0, &[node(0, 1, "7", -1, &[])]);
        let call = node(23, 1, "", 0, &[boxed]);
        let main = node(11, 0, "", -1, &[node(16, 0, "", -1, &[call])]);
        let prefix = format!(
            "loom-checked-1\n3\n0\n-1\n1\n-1\n11\n0\n2\n0\n0\n0\n1\n1\n1\n1\n1\n0\n{identity}1\n0\n0\n0\n0\n0\n0\n{main}1\n0\n0\n"
        );
        let tail = "1\n1\n1\n0\n1\n1\n0\n1\n1\n0\n";
        let program = decode(&format!("{prefix}{tail}")).unwrap();
        assert_eq!(program.interfaces[0].methods[0].result, Type::Int);
        assert_eq!(program.witnesses[0].concrete, Type::Int);
        assert_eq!(program.witnesses[0].methods, [Some(0)]);
        let c::StmtKind::Discard(value) = &program.functions[1].body.statements[0].kind else {
            panic!("expected discard")
        };
        let c::ExprKind::DynCall {
            receiver, slot: 0, ..
        } = &value.kind
        else {
            panic!("expected dyn call")
        };
        assert_eq!(receiver.ty, Type::Dyn(0));
        assert!(matches!(
            receiver.kind,
            c::ExprKind::DynBox { witness: 0, .. }
        ));
        assert!(
            decode(&format!("{prefix}1\n1\n1\n0\n0\n1\n0\n1\n1\n0\n"))
                .unwrap_err()
                .contains("signature mismatch")
        );
        assert!(
            decode(&format!("{prefix}{tail}1\n0\n0\n"))
                .unwrap_err()
                .contains("trailing")
        );
        assert!(decode(&prefix).is_err());
    }
}
