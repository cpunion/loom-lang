use super::*;
use crate::model::{Span, checked::ExprKind as E};

fn value(ty: Type, kind: E) -> checked::Expr {
    checked::Expr {
        ty,
        kind,
        span: Span::default(),
    }
}
fn int(number: i64) -> checked::Expr {
    value(Type::Int, E::Int(number))
}
fn text(text: &str) -> checked::Expr {
    value(Type::Text, E::Text(text.into()))
}
fn local(ty: Type, id: usize) -> checked::Expr {
    value(ty, E::Local(id))
}
fn function(params: Vec<Type>, result: Type, tail: checked::Expr) -> checked::Function {
    checked::Function {
        name: "dyn.test".into(),
        locals: params.clone(),
        params,
        result,
        requires: vec![],
        span: Span::default(),
        body: checked::Block {
            statements: vec![],
            tail: Some(Box::new(tail)),
            falls_through: true,
        },
    }
}
fn invoke(
    interface: usize,
    witness: usize,
    receiver: checked::Expr,
    argument: checked::Expr,
    result: Type,
) -> checked::Expr {
    value(
        result,
        E::DynCall {
            receiver: Box::new(value(
                Type::Dyn(interface),
                E::DynBox {
                    witness,
                    value: Box::new(receiver),
                },
            )),
            slot: 0,
            arguments: vec![argument],
        },
    )
}

#[test]
fn explicit_witnesses_dispatch_and_keep_sparse_reachability() {
    let integer = function(
        vec![Type::Int; 2],
        Type::Int,
        value(
            Type::Int,
            E::Binary(
                Binary::Add,
                Box::new(local(Type::Int, 0)),
                Box::new(local(Type::Int, 1)),
            ),
        ),
    );
    let concatenate = function(
        vec![Type::Text; 2],
        Type::Text,
        value(
            Type::Text,
            E::Primitive(
                Primitive::TextConcat,
                vec![local(Type::Text, 0), local(Type::Text, 1)],
            ),
        ),
    );
    let dead = function(vec![Type::Int], Type::Int, int(999));
    let mut main = function(vec![], Type::Unit, int(0));
    main.body.tail = None;
    for (actual, expected) in [
        (invoke(0, 0, int(40), int(2), Type::Int), int(42)),
        (
            invoke(
                1,
                1,
                value(
                    Type::Text,
                    E::Primitive(Primitive::TextConcat, vec![text("left"), text("")]),
                ),
                value(
                    Type::Text,
                    E::Primitive(Primitive::TextConcat, vec![text("+"), text("right")]),
                ),
                Type::Text,
            ),
            text("left+right"),
        ),
    ] {
        main.body.statements.push(checked::Stmt {
            kind: checked::StmtKind::Assert(value(
                Type::Bool,
                E::Binary(Binary::Eq, Box::new(actual), Box::new(expected)),
            )),
            span: Span::default(),
        });
    }
    let mut program = checked::Program {
        types: vec![],
        lists: vec![],
        functions: vec![integer, concatenate, dead, main],
        entry: Some(3),
        tests: vec![],
        // Public functions can make the source metadata retain full tables,
        // but an executable still starts only at main (or selected tests).
        exports: vec![2],
        interfaces: vec![
            checked::Interface {
                methods: vec![
                    checked::Method {
                        params: vec![Type::Int],
                        result: Type::Int,
                    },
                    checked::Method {
                        params: vec![],
                        result: Type::Int,
                    },
                ],
            },
            checked::Interface {
                methods: vec![checked::Method {
                    params: vec![Type::Text],
                    result: Type::Text,
                }],
            },
        ],
        witnesses: vec![
            checked::Witness {
                interface: 0,
                concrete: Type::Int,
                methods: vec![Some(0), Some(2)],
            },
            checked::Witness {
                interface: 1,
                concrete: Type::Text,
                methods: vec![Some(1)],
            },
            checked::Witness {
                interface: 0,
                concrete: Type::Int,
                methods: vec![Some(0), Some(2)],
            },
        ],
    };
    let Reachable {
        functions,
        witnesses,
        slots,
    } = reachable_functions(&program, &[3], false).unwrap();
    assert_eq!(functions, BTreeSet::from([0, 1, 3]));
    assert_eq!(witnesses, BTreeSet::from([0, 1]));
    assert_eq!(
        slots,
        BTreeSet::from([(Type::Dyn(0), 0), (Type::Dyn(1), 0)])
    );
    assert!(gc::allocating_functions(&program, &functions).contains(&3));
    assert_eq!(value_words(&program, Type::Dyn(0)).unwrap(), 2);
    assert!(gc::managed(&program, Type::Dyn(0)));
    let directory = tempfile::tempdir().unwrap();
    let object = directory.path().join("dyn.o");
    let ir = directory.path().join("dyn.ll");
    let executable = directory
        .path()
        .join(if cfg!(windows) { "dyn.exe" } else { "dyn" });
    let emitted = Llvm
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
    assert!(emitted.uses_runtime);
    let ir = std::fs::read_to_string(ir).unwrap();
    assert!(ir.contains("@loom.witness.0 = private constant { ptr, ptr }"));
    assert!(ir.contains("ptr null"));
    assert!(!ir.contains("@loom.witness.2"));
    assert!(!ir.contains("@loom.fn.2"));
    assert!(ir.contains("@loom_rt_box_new"));
    let runtime = std::env::var_os("LOOM_RUNTIME_LIBRARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join(if cfg!(windows) {
                    "loom_runtime.lib"
                } else {
                    "libloom_runtime.a"
                })
        });
    crate::native_tool::link(&object, &executable, true, Some(&runtime), None).unwrap();
    let output = std::process::Command::new(executable)
        .env("LOOM_GC_STRESS", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        reachable_functions(&program, &[3], true).unwrap().functions,
        BTreeSet::from([0, 1, 2, 3])
    );
    // A used method can itself introduce another call slot: reach a fixed
    // point, rather than freezing the slots discovered in the entry body.
    program.functions[0].body.statements.push(checked::Stmt {
        kind: checked::StmtKind::Discard(value(
            Type::Int,
            E::DynCall {
                receiver: Box::new(value(
                    Type::Dyn(0),
                    E::DynBox {
                        witness: 0,
                        value: Box::new(int(7)),
                    },
                )),
                slot: 1,
                arguments: vec![],
            },
        )),
        span: Span::default(),
    });
    assert_eq!(
        reachable_functions(&program, &[3], false)
            .unwrap()
            .functions,
        BTreeSet::from([0, 1, 2, 3])
    );
    program.functions[0].body.statements.pop();
    program.witnesses[0].methods[1] = None;
    assert!(reachable_functions(&program, &[3], false).is_ok());
    assert!(
        reachable_functions(&program, &[3], true)
            .unwrap_err()
            .to_string()
            .contains("every method")
    );
    program.witnesses[0].methods[0] = None;
    assert!(
        reachable_functions(&program, &[3], false)
            .unwrap_err()
            .to_string()
            .contains("absent witness method")
    );
}
