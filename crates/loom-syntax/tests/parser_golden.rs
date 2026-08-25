use loom_syntax::{
    BinaryOp, BlockItem, DeclKind, ExprKind, ImplKind, PatternKind, TypeExprKind, parse,
};

fn codes(parsed: &loom_syntax::Parse) -> Vec<&str> {
    parsed
        .diagnostics()
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect()
}

fn assert_clean(source: &str) -> loom_syntax::Parse {
    let parsed = parse(source);
    assert!(
        !parsed.has_errors(),
        "unexpected diagnostics: {:#?}",
        parsed.diagnostics()
    );
    assert_eq!(parsed.reconstructed(), source);
    parsed
}

#[test]
fn parses_postfix_await_as_a_chainable_keyword() {
    let parsed = assert_clean(
        "module tasks\nasync fn child() Int { 1 }\nasync fn parent() Int { child().await }\n",
    );
    let DeclKind::Function(parent) = &parsed.ast().declarations[1].kind else {
        panic!("expected parent function");
    };
    let BlockItem::Expr(expression) = &parent.body.items[0] else {
        panic!("expected tail expression");
    };
    assert!(matches!(expression.kind, ExprKind::Await(_)));

    let migrated = parse(
        "module tasks\nasync fn child() Int { 1 }\nasync fn parent() Int { await child() }\n",
    );
    assert!(codes(&migrated).contains(&"UnexpectedToken"));
    assert_eq!(
        migrated.reconstructed(),
        "module tasks\nasync fn child() Int { 1 }\nasync fn parent() Int { await child() }\n"
    );

    let propagated = assert_clean(
        "module tasks\nasync fn child() Int { 1 }\nasync fn parent() Int { child().await? }\n",
    );
    let DeclKind::Function(parent) = &propagated.ast().declarations[1].kind else {
        panic!("expected parent function");
    };
    let BlockItem::Expr(expression) = &parent.body.items[0] else {
        panic!("expected tail expression");
    };
    let ExprKind::Propagate(awaited) = &expression.kind else {
        panic!("expected result propagation");
    };
    assert!(matches!(awaited.kind, ExprKind::Await(_)));

    let forced = parse(
        "module tasks\nasync fn child() Int { 1 }\nasync fn parent() Int { child().await! }\n",
    );
    assert!(codes(&forced).contains(&"UnexpectedToken"));
}

#[test]
fn parses_the_core_surface_end_to_end() {
    let parsed = assert_clean(
        r"module shop.order

import standard.float.is_finite

pub type Price = Float where self >= 0.0

pub record Order {
    subtotal Price
    discount Price

    invariant is_finite(self.subtotal) && self.discount <= self.subtotal
}

pub enum LookupError {
    Missing
    Unavailable(Text)
}

pub fn value_or[T](value Option[T], fallback T) T {
    match value {
        Some(found) => found
        None => fallback
    }
}

impl Order {
    method total(self) Float
        ensures result >= 0.0
    {
        self.subtotal - self.discount
    }

    pub method apply(mut self, value Price) Unit
        requires value <= self.subtotal
        ensures self.discount == value
    {
        self.discount = value
    }
}

test fn total_stays_positive() {
    assert true
}
",
    );
    assert_eq!(parsed.ast().imports.len(), 1);
    assert_eq!(parsed.ast().declarations.len(), 6);
}

#[test]
fn supports_comma_or_newline_in_braced_lists() {
    let parsed = assert_clean(
        r"module lists
record Empty {}
record Pair { left Int, right Int }
enum EmptyEnum {}
enum Choice { One, Two(Text) }
fn choose(value Choice) Int {
    match value { Choice.One => 1, Choice.Two(text) => 2 }
}
",
    );
    let DeclKind::Enum(empty) = &parsed.ast().declarations[2].kind else {
        panic!("expected enum");
    };
    assert!(empty.variants.is_empty());
}

#[test]
fn parses_generic_inherent_impl() {
    let parsed = assert_clean(
        r"module generic
record Pair[T] { value T }
impl[T] Pair[T] {
    method get(self) T { self.value }
}
",
    );
    let DeclKind::Impl(implementation) = &parsed.ast().declarations[1].kind else {
        panic!("expected impl");
    };
    assert_eq!(implementation.generics.len(), 1);
    assert!(matches!(implementation.kind, ImplKind::Inherent { .. }));
}

#[test]
fn parses_static_concepts_conformance_and_simple_dyn() {
    let parsed = assert_clean(
        r"module concepts

pub concept Zero {
    static method zero() Self
}

pub dyn concept Source {
    associated type Item
    method next(mut self) Option[Self.Item]
}

impl Zero for Int {
    static method zero() Int { 0 }
}

fn consume(source dyn Source[Item = Text]) Unit {
    Unit
}
",
    );
    let DeclKind::Function(function) = &parsed.ast().declarations[3].kind else {
        panic!("expected function");
    };
    assert!(matches!(
        function.signature.parameters[0].ty.kind,
        TypeExprKind::BareDyn(_)
    ));
}

#[test]
fn concept_parameter_and_explicit_dyn_are_distinct_ast_nodes() {
    let parsed = assert_clean(
        r"module views
dyn concept Formatter {
    associated type Error
    method format(self, text Text) Result[Text, Self.Error]
}
fn use(
    formatter Formatter[Error = FormatError],
    erased dyn Formatter[Error = FormatError],
) Unit {
    Unit
}
",
    );
    let DeclKind::Function(function) = &parsed.ast().declarations[1].kind else {
        panic!("expected function");
    };
    assert!(matches!(
        function.signature.parameters[0].ty.kind,
        TypeExprKind::Named { .. }
    ));
    assert!(matches!(
        function.signature.parameters[1].ty.kind,
        TypeExprKind::BareDyn(_)
    ));
}

#[test]
fn legacy_view_surface_is_rejected_but_lossless() {
    let source = "module legacy\nfn use(value view[dyn C]) Unit { Unit }\n";
    let parsed = parse(source);
    assert!(codes(&parsed).contains(&"UnexpectedToken"));
    assert_eq!(parsed.reconstructed(), source);
}

#[test]
fn imports_after_declarations_are_diagnosed_without_being_lost() {
    let parsed = parse("module ordering\nrecord A {}\nimport other.B\nrecord C {}\n");
    assert!(codes(&parsed).contains(&"UnexpectedToken"));
    assert_eq!(parsed.ast().imports.len(), 1);
    assert_eq!(parsed.ast().declarations.len(), 2);
}

#[test]
fn constrained_type_requires_where() {
    let parsed = parse("module types\ntype Price = Float\nrecord StillHere {}\n");
    assert!(codes(&parsed).contains(&"UnexpectedToken"));
    assert_eq!(parsed.ast().declarations.len(), 2);
}

#[test]
fn invariant_is_unique_and_must_follow_fields() {
    let parsed =
        parse("module records\nrecord Bad {\n invariant true\n late Int\n invariant false\n}\n");
    let found = codes(&parsed);
    assert!(
        found
            .iter()
            .filter(|code| **code == "UnexpectedToken")
            .count()
            >= 2
    );
}

#[test]
fn requires_must_precede_ensures_but_both_are_retained() {
    let parsed = parse(
        "module contracts\nfn bad() Unit\n ensures result == Unit\n requires true\n{ Unit }\n",
    );
    assert!(codes(&parsed).contains(&"UnexpectedToken"));
    let DeclKind::Function(function) = &parsed.ast().declarations[0].kind else {
        panic!("expected function");
    };
    assert_eq!(function.signature.contracts.len(), 2);
}

#[test]
fn every_callable_may_omit_a_fixed_unit_return() {
    let parsed = assert_clean(
        r"module returns
record R {}

fn private() { return }
pub fn public() {}
async fn asynchronous() { return }
pub async fn publicAsynchronous() {}
test fn testUnit() { return }
test async fn asyncTestUnit() {}
fn explicit() Unit { Unit }
pub fn contracted(flag Bool)
    requires flag
    ensures true
{
    return
}

impl R {
    method privateMethod(self) {}
    pub method publicMethod(self) { return }
}

concept C {
    method required(self)
    static method requiredStatic()
}

impl C for R {
    method required(self) {}
    static method requiredStatic() { return }
}
",
    );
    assert_eq!(parsed.ast().declarations.len(), 12);

    let DeclKind::Function(private) = &parsed.ast().declarations[1].kind else {
        panic!("expected private function");
    };
    assert!(private.signature.return_type.is_none());

    let DeclKind::Function(explicit) = &parsed.ast().declarations[7].kind else {
        panic!("expected explicit Unit function");
    };
    assert!(explicit.signature.return_type.is_some());

    let DeclKind::Function(contracted) = &parsed.ast().declarations[8].kind else {
        panic!("expected contracted function");
    };
    assert!(contracted.signature.return_type.is_none());
    assert_eq!(contracted.signature.contracts.len(), 2);
}

#[test]
fn if_without_else_does_not_consume_the_next_statement_boundary() {
    let parsed =
        assert_clean("module flow\nfn run(flag Bool) Unit {\n if flag { Unit }\n assert true\n}\n");
    let DeclKind::Function(function) = &parsed.ast().declarations[0].kind else {
        panic!("expected function");
    };
    assert_eq!(function.body.items.len(), 2);
}

#[test]
fn if_and_match_require_parentheses_around_record_literal_scrutinees() {
    assert_clean(
        r"module control
record Flag { enabled Bool }
fn check() Unit {
    if (Flag { enabled = true }) { Unit }
    match (Flag { enabled = false }) { _ => Unit }
}
",
    );
    let bad = parse(
        "module control\nrecord Flag { enabled Bool }\nfn check() Unit { if Flag { enabled = true } { Unit } }\n",
    );
    assert!(bad.has_errors());
}

#[test]
fn comparison_precedence_is_stable() {
    let parsed = assert_clean("module precedence\nfn value() Bool { 1 + 2 * 3 == 7 || false }\n");
    let DeclKind::Function(function) = &parsed.ast().declarations[0].kind else {
        panic!("expected function");
    };
    let BlockItem::Expr(expression) = &function.body.items[0] else {
        panic!("expected expression");
    };
    let ExprKind::Binary {
        op: BinaryOp::Or,
        left,
        ..
    } = &expression.kind
    else {
        panic!("expected || at root: {expression:#?}");
    };
    assert!(matches!(
        left.kind,
        ExprKind::Binary {
            op: BinaryOp::Equal,
            ..
        }
    ));
}

#[test]
fn comparisons_are_non_associative() {
    let parsed = parse("module compare\nfn bad(a Int, b Int, c Int) Bool { a < b < c }\n");
    assert!(codes(&parsed).contains(&"ChainedComparison"));
}

#[test]
fn negative_number_is_not_a_literal_pattern() {
    let parsed = parse("module patterns\nfn f(x Int) Int { match x { -1 => 0, _ => 1 } }\n");
    assert!(codes(&parsed).contains(&"UnexpectedToken"));
}

#[test]
fn name_patterns_remain_unresolved_without_casing_heuristics() {
    let parsed = assert_clean(
        "module patterns\nfn f(x Option[Int]) Int { match x { Some(value) => value, none => 0 } }\n",
    );
    let DeclKind::Function(function) = &parsed.ast().declarations[0].kind else {
        panic!("expected function");
    };
    let BlockItem::Expr(expression) = &function.body.items[0] else {
        panic!("expected match");
    };
    let ExprKind::Match { arms, .. } = &expression.kind else {
        panic!("expected match");
    };
    assert!(matches!(arms[0].pattern.kind, PatternKind::Name { .. }));
    assert!(matches!(arms[1].pattern.kind, PatternKind::Name { .. }));
}

#[test]
fn local_type_annotations_are_rejected_locally() {
    let parsed = parse("module locals\nfn f() Unit { let value: Int = 1\n Unit }\n");
    assert!(codes(&parsed).contains(&"UnexpectedToken"));
    assert_eq!(parsed.ast().declarations.len(), 1);
}

#[test]
fn malformed_top_level_declaration_recovers_at_full_start_sequence() {
    let parsed =
        parse("module recovery\nnoise that is not a declaration\npub fn good() Unit { Unit }\n");
    assert!(codes(&parsed).contains(&"UnexpectedToken"));
    let good = parsed
        .ast()
        .declarations
        .iter()
        .find_map(|decl| match &decl.kind {
            DeclKind::Function(function) if function.signature.name.text == "good" => {
                Some(function)
            }
            _ => None,
        });
    assert!(good.is_some(), "{:#?}", parsed.ast().declarations);
}

#[test]
fn top_level_recovery_ignores_an_unclosed_parenthesis() {
    let source = "module recovery\nfn broken(\npub fn good() Unit { Unit }\n";
    let parsed = parse(source);
    assert!(codes(&parsed).contains(&"UnexpectedToken"));
    assert!(parsed.ast().declarations.iter().any(|decl| {
        matches!(&decl.kind, DeclKind::Function(function) if function.signature.name.text == "good")
    }));
    assert_eq!(parsed.reconstructed(), source);
}

#[test]
fn impl_recovery_keeps_the_next_complete_method() {
    let source = r"module recovery
record R {}
impl R {
    method broken(self) Unit {
        return
    method good(self) Unit { Unit }
}
";
    let parsed = parse(source);
    assert!(codes(&parsed).contains(&"UnexpectedToken"));
    let DeclKind::Impl(implementation) = &parsed.ast().declarations[1].kind else {
        panic!("expected impl");
    };
    let ImplKind::Inherent { methods, .. } = &implementation.kind else {
        panic!("expected inherent impl");
    };
    assert!(
        methods
            .iter()
            .any(|method| method.signature.name.text == "good")
    );
}

#[test]
fn impl_recovery_ignores_an_unclosed_method_parameter_list() {
    let source = r"module recovery
record R {}
impl R {
    method broken(
    method good(self) Unit { Unit }
}
";
    let parsed = parse(source);
    assert!(codes(&parsed).contains(&"UnexpectedToken"));
    let DeclKind::Impl(implementation) = &parsed.ast().declarations[1].kind else {
        panic!("expected impl");
    };
    let ImplKind::Inherent { methods, .. } = &implementation.kind else {
        panic!("expected inherent impl");
    };
    assert!(
        methods
            .iter()
            .any(|method| method.signature.name.text == "good")
    );
}

#[test]
fn lexical_errors_are_local_and_later_declarations_survive() {
    let source = "module strings\nfn broken() Text { \"oops\n}\nfn good() Unit { Unit }\n";
    let parsed = parse(source);
    assert!(codes(&parsed).contains(&"NewlineInString"));
    assert!(parsed.ast().declarations.iter().any(|decl| {
        matches!(&decl.kind, DeclKind::Function(function) if function.signature.name.text == "good")
    }));
    assert_eq!(parsed.reconstructed(), source);
}

#[test]
fn missing_module_is_a_stable_diagnostic() {
    let parsed = parse("record R {}\n");
    assert!(codes(&parsed).contains(&"MissingModuleDeclaration"));
    assert_eq!(parsed.ast().declarations.len(), 1);
}

#[test]
fn bare_return_is_syntactically_accepted() {
    let parsed = assert_clean("module returns\nfn stop() { return }\n");
    let DeclKind::Function(function) = &parsed.ast().declarations[0].kind else {
        panic!("expected function");
    };
    let BlockItem::Return(returned) = &function.body.items[0] else {
        panic!("expected return");
    };
    assert!(returned.value.is_none());
}
