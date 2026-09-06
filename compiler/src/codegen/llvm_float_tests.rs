use super::*;
use crate::model::{Span, checked::ExprKind as E};

fn expr(ty: Type, kind: E) -> checked::Expr {
    checked::Expr {
        ty,
        kind,
        span: Span::default(),
    }
}

fn local(ty: Type, index: usize) -> checked::Expr {
    expr(ty, E::Local(index))
}

fn float(value: f64) -> checked::Expr {
    expr(Type::Float, E::Float(value))
}

fn body(tail: checked::Expr) -> checked::Block {
    checked::Block {
        statements: vec![],
        tail: Some(Box::new(tail)),
        falls_through: true,
    }
}

fn function(params: Vec<Type>, result: Type, value: checked::Expr) -> checked::Function {
    checked::Function {
        name: "float.test".into(),
        locals: params.clone(),
        params,
        result,
        requires: vec![],
        body: body(value),
        span: Span::default(),
    }
}

#[test]
fn ieee_float_operations_and_aggregate_payloads_execute_without_runtime() {
    let operations = [
        Binary::Add,
        Binary::Sub,
        Binary::Mul,
        Binary::Div,
        Binary::Rem,
        Binary::Eq,
        Binary::Ne,
        Binary::Lt,
        Binary::Le,
        Binary::Gt,
        Binary::Ge,
    ];
    let mut functions = operations
        .iter()
        .enumerate()
        .map(|(id, operation)| {
            let result = if id < 5 { Type::Float } else { Type::Bool };
            function(
                vec![Type::Float; 2],
                result,
                expr(
                    result,
                    E::Binary(
                        *operation,
                        Box::new(local(Type::Float, 0)),
                        Box::new(local(Type::Float, 1)),
                    ),
                ),
            )
        })
        .collect::<Vec<_>>();
    functions.push(function(
        vec![Type::Float],
        Type::Float,
        expr(
            Type::Float,
            E::Unary(Unary::Neg, Box::new(local(Type::Float, 0))),
        ),
    ));
    functions.push(function(
        vec![Type::Int],
        Type::Float,
        expr(
            Type::Float,
            E::Primitive(Primitive::FloatFromInt, vec![local(Type::Int, 0)]),
        ),
    ));
    functions.push(function(
        vec![Type::Float],
        Type::Int,
        expr(
            Type::Int,
            E::Primitive(Primitive::FloatToInt, vec![local(Type::Float, 0)]),
        ),
    ));

    // A record inside an enum must preserve NaN and the sign of zero bitwise.
    let record = expr(Type::Data(0), E::Record(vec![(0, local(Type::Float, 0))]));
    let variant = expr(
        Type::Data(1),
        E::Variant {
            variant: 0,
            fields: vec![record],
        },
    );
    let matched = expr(
        Type::Float,
        E::Match {
            value: Box::new(variant),
            arms: vec![checked::MatchArm {
                variant: Some(0),
                bindings: vec![Some(1)],
                whole: None,
                body: body(expr(
                    Type::Float,
                    E::Field(Box::new(local(Type::Data(0), 1)), 0),
                )),
            }],
        },
    );
    let mut roundtrip = function(vec![Type::Float], Type::Float, matched);
    roundtrip.locals.push(Type::Data(0));
    functions.push(roundtrip);
    let call = |id: usize, arguments| expr(functions[id].result, E::Call(id, arguments));
    let arithmetic = |id, left, right| call(id, vec![float(left), float(right)]);
    let mut conditions = Vec::new();
    for (id, expected) in [3.75, -0.75, 3.375, 2.0 / 3.0, 1.5].into_iter().enumerate() {
        conditions.push(expr(
            Type::Bool,
            E::Binary(
                Binary::Eq,
                Box::new(arithmetic(id, 1.5, 2.25)),
                Box::new(float(expected)),
            ),
        ));
    }
    let nan = arithmetic(3, 0.0, 0.0);
    for (id, expected) in [
        (5, false),
        (6, true),
        (7, false),
        (8, false),
        (9, false),
        (10, false),
    ] {
        conditions.push(expr(
            Type::Bool,
            E::Binary(
                Binary::Eq,
                Box::new(call(id, vec![nan.clone(), nan.clone()])),
                Box::new(expr(Type::Bool, E::Bool(expected))),
            ),
        ));
    }
    conditions.push(arithmetic(5, -0.0, 0.0));
    for value in [call(11, vec![float(0.0)]), call(14, vec![float(-0.0)])] {
        let reciprocal = call(3, vec![float(1.0), value]);
        conditions.push(expr(
            Type::Bool,
            E::Binary(
                Binary::Eq,
                Box::new(reciprocal),
                Box::new(float(f64::NEG_INFINITY)),
            ),
        ));
    }
    let recovered_nan = call(14, vec![nan.clone()]);
    conditions.push(call(6, vec![recovered_nan.clone(), recovered_nan]));
    for (value, expected) in [
        (i64::MIN, -9223372036854775808.0),
        (i64::MAX, 9223372036854775808.0),
    ] {
        conditions.push(expr(
            Type::Bool,
            E::Binary(
                Binary::Eq,
                Box::new(call(12, vec![expr(Type::Int, E::Int(value))])),
                Box::new(float(expected)),
            ),
        ));
    }
    for (value, expected) in [
        (nan, 0),
        (float(f64::INFINITY), i64::MAX),
        (float(f64::NEG_INFINITY), i64::MIN),
        (float(-1.75), -1),
    ] {
        conditions.push(expr(
            Type::Bool,
            E::Binary(
                Binary::Eq,
                Box::new(call(13, vec![value])),
                Box::new(expr(Type::Int, E::Int(expected))),
            ),
        ));
    }
    let entry = functions.len();
    functions.push(checked::Function {
        name: "main".into(),
        params: vec![],
        result: Type::Unit,
        locals: vec![],
        requires: vec![],
        span: Span::default(),
        body: checked::Block {
            statements: conditions
                .into_iter()
                .map(|condition| checked::Stmt {
                    kind: checked::StmtKind::Assert(condition),
                    span: Span::default(),
                })
                .collect(),
            tail: None,
            falls_through: true,
        },
    });
    let program = checked::Program {
        function_types: vec![],
        interfaces: vec![],
        witnesses: vec![],
        types: vec![
            checked::Data {
                name: "Number".into(),
                kind: checked::DataKind::Record(vec![("value".into(), Type::Float)]),
            },
            checked::Data {
                name: "Wrapped".into(),
                kind: checked::DataKind::Enum(vec![("Number".into(), vec![Type::Data(0)])]),
            },
        ],
        lists: vec![],
        functions,
        entry: Some(entry),
        tests: vec![],
        exports: vec![],
    };
    assert_eq!(value_words(&program, Type::Data(1)).unwrap(), 2);
    assert!(!gc::managed(&program, Type::Data(1)));
    let directory = tempfile::tempdir().unwrap();
    let object = directory.path().join("float.o");
    let ir = directory.path().join("float.ll");
    let executable = directory
        .path()
        .join(if cfg!(windows) { "float.exe" } else { "float" });
    let emission = Llvm
        .emit(
            &program,
            EmitOptions {
                object: &object,
                ir: Some(&ir),
                test_mode: false,
                optimization: Optimization::O0,
            },
        )
        .unwrap();
    assert!(!emission.uses_runtime);
    let ir = std::fs::read_to_string(ir).unwrap();
    for operation in [
        "fadd double",
        "fsub double",
        "fmul double",
        "fdiv double",
        "frem double",
        "fneg double",
        "fcmp oeq double",
        "fcmp une double",
        "fcmp olt double",
        "fcmp ole double",
        "fcmp ogt double",
        "fcmp oge double",
        "sitofp i64",
        "llvm.fptosi.sat.i64.f64",
        "bitcast double",
    ] {
        assert!(ir.contains(operation), "missing {operation}");
    }
    assert!(!ir.contains(" fast "));
    crate::native_tool::link(&object, &executable, false, None, None).unwrap();
    let output = std::process::Command::new(executable).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
