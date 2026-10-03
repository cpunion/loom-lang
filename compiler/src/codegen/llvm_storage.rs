//! Conservative, flow-insensitive escape analysis of checked storage. Aliases,
//! containers and captures share a component. Direct calls compose finite alias,
//! publication and result summaries; unknown calls publish their arguments.
//! Returning local storage does not publish it during this invocation.
//! GC roots and cancellation checkpoints are independent of this analysis.

use super::*;

pub(super) type Forwarders = HashMap<usize, Primitive>;

#[derive(Default, PartialEq, Eq)]
struct Summary {
    aliases: Vec<(usize, usize)>,
    published: BTreeSet<usize>,
    result_params: BTreeSet<usize>,
    result_shared: bool,
}

type Summaries = HashMap<usize, Summary>;

#[derive(Default)]
pub(super) struct StoragePlan {
    pub functions: HashMap<usize, PrivateStorage>,
    pub forwarders: Forwarders,
}

impl StoragePlan {
    pub fn analyze(
        program: &checked::Program,
        reachable: &BTreeSet<usize>,
        roots: &[usize],
        witnesses: &BTreeSet<usize>,
    ) -> Self {
        // Least fixed point: facts only grow. Recursion contributes exactly the
        // aliases/publications reachable in its checked bodies, not a purity
        // promise or a sampled depth. Unknown/native/dynamic calls stay opaque.
        let mut summaries: Summaries = reachable
            .iter()
            .map(|id| (*id, Summary::default()))
            .collect();
        loop {
            let mut changed = false;
            for id in reachable {
                let source = &program.functions[*id];
                let mut analysis = Analysis::new(program, source, &summaries, &[]);
                analysis.function(source);
                let summary = analysis.summary(source);
                if summaries[id] != summary {
                    summaries.insert(*id, summary);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        // A single body is emitted, so a parameter is private only if *every*
        // incoming context is private. References, witnesses and exported entry
        // points can receive arbitrary storage; they never inherit local facts.
        let mut shared_params: HashMap<usize, Vec<bool>> = reachable
            .iter()
            .map(|id| (*id, vec![false; program.functions[*id].params.len()]))
            .collect();
        let mut unknown = roots.iter().copied().collect::<BTreeSet<_>>();
        for id in reachable {
            let source = &program.functions[*id];
            let mut values = Vec::new();
            for requirement in &source.requires {
                gc::expressions(requirement, &mut values);
            }
            gc::block_expressions(&source.body, &mut values);
            for value in values {
                match value.kind {
                    checked::ExprKind::FunctionRef(target)
                    | checked::ExprKind::Closure {
                        function: target, ..
                    } => {
                        unknown.insert(target);
                    }
                    _ => {}
                }
            }
        }
        for id in witnesses {
            unknown.extend(program.witnesses[*id].methods.iter().flatten());
        }
        for id in unknown {
            if let Some(params) = shared_params.get_mut(&id) {
                params.fill(true);
            }
        }
        loop {
            let mut changed = false;
            for id in reachable {
                let source = &program.functions[*id];
                let mut analysis = Analysis::new(program, source, &summaries, &shared_params[id]);
                analysis.function(source);
                for (target, args) in std::mem::take(&mut analysis.calls) {
                    if let Some(params) = shared_params.get_mut(&target) {
                        for (index, node) in args.into_iter().enumerate() {
                            if let Some(node) = node {
                                let root = analysis.root(node);
                                if analysis.escaped[root] && !params[index] {
                                    params[index] = true;
                                    changed = true;
                                }
                            }
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }
        Self {
            forwarders: forwarders(program, reachable),
            functions: reachable
                .iter()
                .map(|id| {
                    let source = &program.functions[*id];
                    let mut analysis =
                        Analysis::new(program, source, &summaries, &shared_params[id]);
                    analysis.function(source);
                    (*id, analysis.private())
                })
                .collect(),
        }
    }
}

// Only identity forwarding with no guards or extra work can become the same
// primitive at a private call site. Source names confer no special behavior.
pub(super) fn forwarders(program: &checked::Program, reachable: &BTreeSet<usize>) -> Forwarders {
    let mut output = Forwarders::new();
    loop {
        let before = output.len();
        for id in reachable {
            let source = &program.functions[*id];
            if output.contains_key(id) || !source.requires.is_empty() {
                continue;
            }
            let value = match (source.body.statements.as_slice(), &source.body.tail) {
                ([], Some(value)) => value.as_ref(),
                ([statement], None) => match &statement.kind {
                    checked::StmtKind::Return(Some(value))
                    | checked::StmtKind::Expr(value)
                    | checked::StmtKind::Discard(value) => value,
                    _ => continue,
                },
                _ => continue,
            };
            if value.ty != source.result {
                // Discarding a primitive result is not identity forwarding.
                continue;
            }
            let (operation, args) = match &value.kind {
                checked::ExprKind::Primitive(operation, args) if local_operation(*operation) => {
                    (*operation, args)
                }
                checked::ExprKind::Call(target, args) => match output.get(target) {
                    Some(operation) => (*operation, args),
                    None => continue,
                },
                _ => continue,
            };
            if args.len() == source.params.len() && args.iter().enumerate().all(|(index, arg)| {
                matches!(arg.kind, checked::ExprKind::Local(local) if local == index)
            }) {
                output.insert(*id, operation);
            }
        }
        if output.len() == before {
            return output;
        }
    }
}

fn local_operation(operation: Primitive) -> bool {
    matches!(
        operation,
        Primitive::ListNew
            | Primitive::ListNewCapacity
            | Primitive::ListLen
            | Primitive::ListGet
            | Primitive::ListSet
            | Primitive::ListPush
            | Primitive::ListPop
            | Primitive::ListRetainRange
            | Primitive::BytesNew
            | Primitive::BytesTextCopy
            | Primitive::BytesLen
            | Primitive::BytesGet
            | Primitive::BytesSet
            | Primitive::BytesPush
            | Primitive::BytesUtf8
    )
}

fn storage(program: &checked::Program, ty: Type) -> bool {
    match ty {
        Type::List(_) | Type::Bytes | Type::Function(_) | Type::Dyn(_) => true,
        Type::Data(id) => match &program.types[id].kind {
            checked::DataKind::Frame(_) | checked::DataKind::Task(_) => true,
            checked::DataKind::Refined(ty) => storage(program, *ty),
            checked::DataKind::Record(fields) => fields.iter().any(|(_, ty)| storage(program, *ty)),
            checked::DataKind::Enum(variants) => variants
                .iter()
                .any(|(_, fields)| fields.iter().any(|ty| storage(program, *ty))),
        },
        _ => false,
    }
}

#[derive(Default)]
pub(super) struct PrivateStorage {
    expressions: BTreeSet<usize>,
}

impl PrivateStorage {
    pub fn contains(&self, value: &checked::Expr) -> bool {
        self.expressions
            .contains(&(value as *const checked::Expr as usize))
    }
}

struct Analysis<'a> {
    program: &'a checked::Program,
    summaries: &'a Summaries,
    parents: Vec<usize>,
    escaped: Vec<bool>,
    values: HashMap<usize, usize>,
    calls: Vec<(usize, Vec<Option<usize>>)>,
    returned: Option<usize>,
}

impl Analysis<'_> {
    fn new<'a>(
        program: &'a checked::Program,
        source: &checked::Function,
        summaries: &'a Summaries,
        shared_params: &[bool],
    ) -> Analysis<'a> {
        let mut analysis = Analysis {
            program,
            summaries,
            parents: Vec::new(),
            escaped: Vec::new(),
            values: HashMap::new(),
            calls: Vec::new(),
            returned: None,
        };
        for index in 0..source.locals.len() {
            analysis.node(shared_params.get(index).copied().unwrap_or(false));
        }
        analysis
    }

    fn function(&mut self, source: &checked::Function) {
        for value in &source.requires {
            self.expr(value);
        }
        let value = self.block(&source.body);
        self.returned = self.join(self.returned, value);
    }

    fn summary(&mut self, source: &checked::Function) -> Summary {
        let mut result = Summary::default();
        let returned = self.returned.map(|node| self.root(node));
        result.result_shared = returned.is_some_and(|root| self.escaped[root]);
        for (index, ty) in source.params.iter().enumerate() {
            if !storage(self.program, *ty) {
                continue;
            }
            let root = self.root(index);
            if self.escaped[root] {
                result.published.insert(index);
            }
            if Some(root) == returned {
                result.result_params.insert(index);
            }
            for (other, ty) in source.params.iter().enumerate().take(index) {
                if storage(self.program, *ty) && self.root(other) == root {
                    result.aliases.push((other, index));
                }
            }
        }
        result
    }

    fn private(&mut self) -> PrivateStorage {
        let mut result = PrivateStorage::default();
        for (value, node) in std::mem::take(&mut self.values) {
            let root = self.root(node);
            if !self.escaped[root] {
                result.expressions.insert(value);
            }
        }
        result
    }

    fn node(&mut self, escaped: bool) -> usize {
        let id = self.parents.len();
        self.parents.push(id);
        self.escaped.push(escaped);
        id
    }

    fn root(&mut self, id: usize) -> usize {
        let parent = self.parents[id];
        if parent != id {
            self.parents[id] = self.root(parent);
        }
        self.parents[id]
    }

    fn join(&mut self, left: Option<usize>, right: Option<usize>) -> Option<usize> {
        match (left, right) {
            (Some(left), Some(right)) => {
                let left = self.root(left);
                let right = self.root(right);
                // Stable smaller roots avoid dependence on hash iteration.
                let (left, right) = (left.min(right), left.max(right));
                self.parents[right] = left;
                self.escaped[left] |= self.escaped[right];
                Some(left)
            }
            _ => left.or(right),
        }
    }

    fn publish(&mut self, node: Option<usize>) {
        if let Some(node) = node {
            let root = self.root(node);
            self.escaped[root] = true;
        }
    }

    fn values<'a>(&mut self, values: impl IntoIterator<Item = &'a checked::Expr>) -> Option<usize> {
        let mut result = None;
        for value in values {
            let node = self.expr(value);
            result = self.join(result, node);
        }
        result
    }

    fn primitive(&mut self, operation: Primitive, args: &[checked::Expr]) -> Option<usize> {
        let values = self.values(args);
        if local_operation(operation) {
            Some(values.unwrap_or_else(|| self.node(false)))
        } else {
            self.publish(values);
            Some(self.node(true))
        }
    }

    fn expr(&mut self, value: &checked::Expr) -> Option<usize> {
        use checked::ExprKind as E;
        let node = match &value.kind {
            E::Local(local) => Some(*local),
            E::Bytes(_) | E::FunctionRef(_) => Some(self.node(false)),
            E::Unary(_, inner)
            | E::Coerce(inner)
            | E::Field(inner, _)
            | E::DynBox { value: inner, .. }
            | E::Closure {
                environment: inner, ..
            } => self.expr(inner),
            E::Binary(_, left, right) => self.values([left.as_ref(), right.as_ref()]),
            E::Call(target, args) => {
                let args: Vec<_> = args.iter().map(|arg| self.expr(arg)).collect();
                self.calls.push((*target, args.clone()));
                if let Some(summary) = self.summaries.get(target) {
                    for (left, right) in &summary.aliases {
                        self.join(args[*left], args[*right]);
                    }
                    for index in &summary.published {
                        self.publish(args[*index]);
                    }
                    let mut result = Some(self.node(summary.result_shared));
                    for index in &summary.result_params {
                        result = self.join(result, args[*index]);
                    }
                    result
                } else {
                    for arg in args {
                        self.publish(arg);
                    }
                    Some(self.node(true))
                }
            }
            E::Primitive(operation, args) => self.primitive(*operation, args),
            E::IndirectCall {
                callee: receiver,
                arguments,
            }
            | E::DynCall {
                receiver,
                arguments,
                ..
            } => {
                let args = self.values(std::iter::once(receiver.as_ref()).chain(arguments));
                self.publish(args);
                Some(self.node(true))
            }
            E::List(fields) | E::Variant { fields, .. } => {
                let values = self.values(fields);
                Some(values.unwrap_or_else(|| self.node(false)))
            }
            E::Record(fields) | E::FrameNew(fields) => {
                let values = self.values(fields.iter().map(|(_, value)| value));
                Some(values.unwrap_or_else(|| self.node(false)))
            }
            E::FrameStore { frame, value, .. } => self.values([frame.as_ref(), value.as_ref()]),
            E::Block(body) => self.block(body),
            E::If {
                condition,
                then_body,
                else_body,
            } => {
                self.expr(condition);
                let left = self.block(then_body);
                let right = else_body.as_ref().and_then(|body| self.block(body));
                self.join(left, right)
            }
            E::Match { value, arms } => {
                let matched = self.expr(value);
                let mut result = None;
                for arm in arms {
                    for local in arm.bindings.iter().chain([&arm.whole]).flatten() {
                        self.join(Some(*local), matched);
                    }
                    let value = self.block(&arm.body);
                    result = self.join(result, value);
                }
                result
            }
            _ => None,
        };
        if storage(self.program, value.ty) {
            let node = node.unwrap_or_else(|| self.node(true));
            self.values
                .insert(value as *const checked::Expr as usize, node);
            Some(node)
        } else {
            None
        }
    }

    fn block(&mut self, body: &checked::Block) -> Option<usize> {
        use checked::StmtKind as S;
        for statement in &body.statements {
            match &statement.kind {
                S::Let { local, value } | S::Assign { local, value } => {
                    let value = self.expr(value);
                    self.join(Some(*local), value);
                }
                S::Return(Some(value)) => {
                    let value = self.expr(value);
                    self.returned = self.join(self.returned, value);
                }
                S::Assert {
                    condition: value, ..
                }
                | S::Discard(value)
                | S::Expr(value) => {
                    self.expr(value);
                }
                S::While { condition, body } => {
                    self.expr(condition);
                    self.block(body);
                }
                S::Defer { body, .. } | S::Cleanup { body, .. } => {
                    self.block(body);
                }
                _ => {}
            }
        }
        body.tail.as_ref().and_then(|value| self.expr(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expr(kind: checked::ExprKind, ty: Type) -> checked::Expr {
        checked::Expr {
            kind,
            ty,
            span: Default::default(),
        }
    }

    fn local(id: usize) -> checked::Expr {
        expr(checked::ExprKind::Local(id), Type::List(0))
    }

    fn statement(kind: checked::StmtKind) -> checked::Stmt {
        checked::Stmt {
            kind,
            span: Default::default(),
        }
    }

    #[test]
    fn aliases_and_nested_publication_are_flow_insensitive() {
        use checked::{ExprKind as E, StmtKind as S};
        let program = checked::Program {
            types: vec![],
            lists: vec![Type::Int],
            functions: vec![],
            function_types: vec![],
            interfaces: vec![],
            witnesses: vec![],
            entry: None,
            tests: vec![],
            test_names: vec![],
            target_inputs: vec![],
            exports: vec![],
        };
        for published in [false, true] {
            let mut statements = vec![
                statement(S::Let {
                    local: 0,
                    value: expr(E::List(vec![]), Type::List(0)),
                }),
                statement(S::Let {
                    local: 1,
                    value: local(0),
                }),
                statement(S::Discard(expr(
                    E::Primitive(Primitive::ListLen, vec![local(1)]),
                    Type::Int,
                ))),
            ];
            if published {
                // Even an earlier read must keep its lock when an arbitrary
                // later/loop iteration can publish a nested alias.
                statements.push(statement(S::Discard(expr(
                    E::Call(0, vec![expr(E::List(vec![local(1)]), Type::List(0))]),
                    Type::Unit,
                ))));
            }
            let source = checked::Function {
                name: "example".into(),
                params: vec![],
                result: Type::Int,
                locals: vec![Type::List(0); 2],
                requires: vec![],
                body: checked::Block {
                    statements,
                    tail: Some(Box::new(local(0))),
                    falls_through: true,
                },
                span: Default::default(),
            };
            let summaries = Summaries::new();
            let mut analysis = Analysis::new(&program, &source, &summaries, &[]);
            analysis.function(&source);
            let private = analysis.private();
            let S::Discard(read) = &source.body.statements[2].kind else {
                unreachable!()
            };
            let E::Primitive(_, args) = &read.kind else {
                unreachable!()
            };
            assert_eq!(private.contains(&args[0]), !published);
            assert_eq!(
                private.contains(source.body.tail.as_ref().unwrap()),
                !published
            );
        }
        let discarded = checked::Function {
            name: "discarded".into(),
            params: vec![Type::List(0)],
            result: Type::Unit,
            locals: vec![Type::List(0)],
            requires: vec![],
            body: checked::Block {
                statements: vec![statement(S::Discard(expr(
                    E::Primitive(Primitive::ListLen, vec![local(0)]),
                    Type::Int,
                )))],
                tail: None,
                falls_through: true,
            },
            span: Default::default(),
        };
        let program = checked::Program {
            functions: vec![discarded],
            ..program
        };
        assert!(forwarders(&program, &BTreeSet::from([0])).is_empty());
    }
}
