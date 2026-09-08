use super::*;
use crate::model::{Span, checked::ExprKind as E, checked::StmtKind as S};

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
fn text(string: &str) -> checked::Expr {
    value(Type::Text, E::Text(string.into()))
}
fn local(ty: Type, index: usize) -> checked::Expr {
    value(ty, E::Local(index))
}
fn primitive(ty: Type, operation: Primitive, arguments: Vec<checked::Expr>) -> checked::Expr {
    value(ty, E::Primitive(operation, arguments))
}
fn statement(kind: S) -> checked::Stmt {
    checked::Stmt {
        kind,
        span: Span::default(),
    }
}
fn effect(operation: Primitive, args: Vec<checked::Expr>) -> checked::Stmt {
    statement(S::Expr(primitive(Type::Unit, operation, args)))
}
fn equal(actual: checked::Expr, expected: checked::Expr) -> checked::Stmt {
    statement(S::Assert {
        condition: value(
            Type::Bool,
            E::Binary(Binary::Eq, Box::new(actual), Box::new(expected)),
        ),
        message: String::new(),
    })
}
fn program(locals: Vec<Type>, statements: Vec<checked::Stmt>) -> checked::Program {
    checked::Program {
        functions: vec![checked::Function {
            name: "memory.test".into(),
            params: vec![],
            result: Type::Unit,
            locals,
            requires: vec![],
            span: Span::default(),
            body: checked::Block {
                statements,
                tail: None,
                falls_through: true,
            },
        }],
        lists: vec![Type::Int, Type::Bool, Type::Float, Type::Data(0)],
        types: vec![checked::Data {
            name: "Item".into(),
            kind: checked::DataKind::Record(vec![
                ("text".into(), Type::Text),
                ("number".into(), Type::Int),
            ]),
        }],
        function_types: vec![],
        interfaces: vec![],
        witnesses: vec![],
        entry: Some(0),
        tests: vec![],
        test_names: vec![],
        exports: vec![],
    }
}
fn emit_run(
    program: &checked::Program,
    optimization: Optimization,
) -> (String, std::process::Output) {
    let directory = tempfile::tempdir().unwrap();
    let object = directory.path().join("memory.o");
    let ir = directory.path().join("memory.ll");
    let executable = directory.path().join(if cfg!(windows) {
        "memory.exe"
    } else {
        "memory"
    });
    let emission = Llvm
        .emit(
            program,
            EmitOptions {
                object: &object,
                ir: Some(&ir),
                test_mode: false,
                optimization,
            },
        )
        .unwrap();
    let archive = emission.uses_runtime.then(|| {
        std::env::var_os("LOOM_RUNTIME_LIBRARY")
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
            })
    });
    crate::native_tool::link(
        &object,
        &executable,
        emission.uses_runtime,
        archive.as_deref(),
        None,
    )
    .unwrap();
    let output = std::process::Command::new(executable)
        .env("LOOM_GC_STRESS", "1")
        .output()
        .unwrap();
    (std::fs::read_to_string(ir).unwrap(), output)
}

#[test]
fn match_payload_handoffs_survive_later_allocation_without_losing_other_local_roots() {
    let concat = |left, right| primitive(Type::Text, Primitive::TextConcat, vec![left, right]);
    let matched = |id, statements| {
        value(
            Type::Text,
            E::Match {
                value: Box::new(value(
                    Type::Data(1),
                    E::Variant {
                        variant: 0,
                        fields: vec![concat(text("left"), text("-"))],
                    },
                )),
                arms: vec![checked::MatchArm {
                    variant: Some(0),
                    bindings: vec![Some(id)],
                    whole: None,
                    body: checked::Block {
                        statements,
                        tail: Some(Box::new(local(Type::Text, id))),
                        falls_through: true,
                    },
                }],
            },
        )
    };
    let mut source = program(
        vec![Type::Text; 3],
        vec![
            // Local 0 needs no permanent root: the match's result snapshot
            // protects it while the second operand allocates and moves it.
            equal(
                concat(matched(0, vec![]), concat(text("right"), text("!"))),
                text("left-right!"),
            ),
            // Local 1 must remain rooted across a safe point inside its arm.
            equal(
                matched(
                    1,
                    vec![statement(S::Discard(concat(
                        text("allocate"),
                        text("inside"),
                    )))],
                ),
                text("left-"),
            ),
            statement(S::Discard(matched(2, vec![]))),
            statement(S::Discard(concat(text("allocate"), text("after")))),
            // Out-of-arm reads are legal in checked IR; don't elide this root.
            equal(local(Type::Text, 2), text("left-")),
        ],
    );
    source.types.push(checked::Data {
        name: "Wrapped".into(),
        kind: checked::DataKind::Enum(vec![("Value".into(), vec![Type::Text])]),
    });
    for optimization in [Optimization::O0, Optimization::O2] {
        let (_, output) = emit_run(&source, optimization);
        assert!(
            output.status.success(),
            "{optimization:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn typed_memory_access_preserves_aliases_argument_order_and_managed_layouts() {
    let list = Type::List(0);
    let mut statements = vec![
        statement(S::Let {
            local: 0,
            value: primitive(list, Primitive::ListNew, vec![]),
        }),
        statement(S::Let {
            local: 1,
            value: local(list, 0),
        }),
    ];
    for index in 0..8 {
        statements.push(effect(
            Primitive::ListPush,
            vec![local(list, 0), int(index)],
        ));
    }
    let after_push = |item, result| {
        value(
            Type::Int,
            E::Block(checked::Block {
                statements: vec![effect(Primitive::ListPush, vec![local(list, 1), int(item)])],
                tail: Some(Box::new(int(result))),
                falls_through: true,
            }),
        )
    };
    // Index evaluation grows the same shared list; value evaluation mutates it
    // again. Computing the slot before either argument would use stale state.
    statements.push(effect(
        Primitive::ListSet,
        vec![local(list, 0), after_push(88, 8), after_push(99, 77)],
    ));
    statements.push(equal(
        primitive(Type::Int, Primitive::ListLen, vec![local(list, 1)]),
        int(10),
    ));
    statements.push(equal(
        primitive(Type::Int, Primitive::ListGet, vec![local(list, 0), int(8)]),
        int(77),
    ));
    statements.push(equal(
        primitive(Type::Int, Primitive::ListGet, vec![local(list, 1), int(9)]),
        int(99),
    ));
    let record = |label: &str, number| {
        value(
            Type::Data(0),
            E::Record(vec![
                (
                    0,
                    primitive(
                        Type::Text,
                        Primitive::TextConcat,
                        vec![text(label), text("!")],
                    ),
                ),
                (1, int(number)),
            ]),
        )
    };
    let items = [
        (
            Type::Bool,
            value(Type::Bool, E::Bool(true)),
            value(Type::Bool, E::Bool(false)),
        ),
        (
            Type::Float,
            value(Type::Float, E::Float(1.25)),
            value(Type::Float, E::Float(-2.5)),
        ),
        (Type::Data(0), record("first", 12), record("second", 34)),
    ];
    let mut locals = vec![list, list];
    for (offset, (element, first, second)) in items.into_iter().enumerate() {
        let ty = Type::List(offset + 1);
        let slot = locals.len();
        locals.push(ty);
        statements.push(statement(S::Let {
            local: slot,
            value: primitive(ty, Primitive::ListNew, vec![]),
        }));
        statements.push(effect(
            Primitive::ListPush,
            vec![local(ty, slot), first.clone()],
        ));
        statements.push(effect(
            Primitive::ListPush,
            vec![local(ty, slot), second.clone()],
        ));
        let checks = |index| {
            let actual = primitive(
                element,
                Primitive::ListGet,
                vec![local(ty, slot), int(index)],
            );
            if element == Type::Data(0) {
                vec![
                    equal(
                        value(Type::Text, E::Field(Box::new(actual.clone()), 0)),
                        text("second!"),
                    ),
                    equal(value(Type::Int, E::Field(Box::new(actual), 1)), int(34)),
                ]
            } else {
                vec![equal(actual, second.clone())]
            }
        };
        statements.extend(checks(1));
        statements.push(effect(
            Primitive::ListSet,
            vec![local(ty, slot), int(0), second.clone()],
        ));
        statements.extend(checks(0));
    }
    let bytes = locals.len();
    locals.push(Type::Bytes);
    statements.push(statement(S::Let {
        local: bytes,
        value: primitive(Type::Bytes, Primitive::BytesNew, vec![]),
    }));
    for byte in [195, 169].into_iter().cycle().take(16) {
        statements.push(effect(
            Primitive::BytesPush,
            vec![local(Type::Bytes, bytes), int(byte)],
        ));
    }
    statements.push(equal(
        primitive(
            Type::Int,
            Primitive::BytesLen,
            vec![local(Type::Bytes, bytes)],
        ),
        int(16),
    ));
    let copy = primitive(
        Type::Text,
        Primitive::BytesTextCopy,
        vec![local(Type::Bytes, bytes)],
    );
    statements.push(equal(
        primitive(Type::Int, Primitive::TextLen, vec![copy.clone()]),
        int(16),
    ));
    statements.push(equal(
        primitive(Type::Int, Primitive::TextByte, vec![copy, int(1)]),
        int(169),
    ));
    let program = program(locals, statements);
    for optimization in [Optimization::O0, Optimization::O3] {
        let (ir, output) = emit_run(&program, optimization);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        for obsolete in [
            "text_len",
            "text_byte",
            "bytes_len",
            "list_len",
            "list_get",
            "list_push",
            "bytes_push",
        ] {
            assert!(
                !ir.contains(&format!("@loom_rt_{obsolete}(")),
                "opaque accessor: {obsolete}"
            );
        }
        assert!(ir.contains("@loom_rt_list_reserve_one("));
        assert!(ir.contains("@loom_rt_bytes_reserve_one("));
        assert!(!ir.contains("list.item"));
        if optimization == Optimization::O0 {
            assert!(ir.contains("buffer.grow"));
            assert!(ir.contains("buffer.full = icmp eq"));
        }
    }
}

#[test]
fn direct_access_rejects_invalid_indices_and_bytes() {
    let list = Type::List(0);
    for operation in [
        Primitive::TextByte,
        Primitive::ListGet,
        Primitive::ListSet,
        Primitive::BytesPush,
    ] {
        let bytes = operation == Primitive::BytesPush;
        for index in [-1, if bytes { 256 } else { 1 }] {
            let mut statements = vec![];
            let mut args = if operation == Primitive::TextByte {
                vec![text("x"), int(index)]
            } else if bytes {
                vec![
                    primitive(Type::Bytes, Primitive::BytesNew, vec![]),
                    int(index),
                ]
            } else {
                statements.push(statement(S::Let {
                    local: 0,
                    value: primitive(list, Primitive::ListNew, vec![]),
                }));
                statements.push(effect(Primitive::ListPush, vec![local(list, 0), int(42)]));
                vec![local(list, 0), int(index)]
            };
            statements.push(if bytes {
                effect(operation, args)
            } else if operation == Primitive::ListSet {
                args.push(int(99));
                effect(operation, args)
            } else {
                statement(S::Discard(primitive(Type::Int, operation, args)))
            });
            let (_, output) = emit_run(&program(vec![list], statements), Optimization::O3);
            assert!(!output.status.success());
            let message = if operation == Primitive::TextByte {
                "text byte index out of bounds"
            } else if bytes {
                "byte value out of range"
            } else {
                "list index out of bounds"
            };
            assert!(String::from_utf8_lossy(&output.stderr).contains(message));
        }
    }
}
