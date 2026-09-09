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
fn diagnostic_text(bytes: &[u8]) -> String {
    // Host CRT diagnostics may use CRLF; binary I/O remains byte-exact.
    std::str::from_utf8(bytes).unwrap().replace("\r\n", "\n")
}
fn local(ty: Type, index: usize) -> checked::Expr {
    value(ty, E::Local(index))
}
fn reference(signature: usize, target: usize) -> checked::Expr {
    value(Type::Function(signature), E::FunctionRef(target))
}
fn invoke(ty: Type, callee: checked::Expr, arguments: Vec<checked::Expr>) -> checked::Expr {
    value(
        ty,
        E::IndirectCall {
            callee: Box::new(callee),
            arguments,
        },
    )
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
fn equal(actual: checked::Expr, expected: checked::Expr) -> checked::Stmt {
    statement(S::Assert {
        condition: value(
            Type::Bool,
            E::Binary(Binary::Eq, Box::new(actual), Box::new(expected)),
        ),
        message: String::new(),
    })
}
fn body(statements: Vec<checked::Stmt>, tail: Option<checked::Expr>) -> checked::Block {
    checked::Block {
        statements,
        tail: tail.map(Box::new),
        falls_through: true,
    }
}
fn function(params: Vec<Type>, result: Type, tail: Option<checked::Expr>) -> checked::Function {
    checked::Function {
        name: "function.value.test".into(),
        locals: params.clone(),
        params,
        result,
        requires: vec![],
        span: Span::default(),
        body: body(vec![], tail),
    }
}
fn program(
    functions: Vec<checked::Function>,
    function_types: Vec<checked::Signature>,
) -> checked::Program {
    checked::Program {
        functions,
        function_types,
        types: vec![],
        lists: vec![],
        interfaces: vec![],
        witnesses: vec![],
        entry: Some(2),
        tests: vec![],
        test_names: vec![],
        exports: vec![],
    }
}
fn emit_run(
    program: &checked::Program,
    test_mode: bool,
    runtime: bool,
) -> (String, std::process::Output) {
    emit_run_at(program, test_mode, runtime, Optimization::O0)
}

fn emit_run_at(
    program: &checked::Program,
    test_mode: bool,
    runtime: bool,
    optimization: Optimization,
) -> (String, std::process::Output) {
    let directory = tempfile::tempdir().unwrap();
    let object = directory.path().join("functions.o");
    let ir = directory.path().join("functions.ll");
    let executable = directory.path().join(if cfg!(windows) {
        "functions.exe"
    } else {
        "functions"
    });
    let emission = Llvm
        .emit(
            program,
            EmitOptions {
                object: &object,
                ir: Some(&ir),
                test_mode,
                optimization,
            },
        )
        .unwrap();
    assert_eq!(emission.uses_runtime, runtime);
    let archive = runtime.then(|| {
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
    crate::native_tool::link(&object, &executable, runtime, archive.as_deref(), None).unwrap();
    let output = std::process::Command::new(executable)
        .env("LOOM_GC_STRESS", "1")
        .output()
        .unwrap();
    (std::fs::read_to_string(ir).unwrap(), output)
}

#[test]
fn captured_functions_escape_share_state_and_survive_container_gc() {
    let callable = Type::Function(0);
    let environment = Type::Data(0);
    let wrapper = Type::Data(1);
    let variant = Type::Data(2);
    let list = Type::List(0);
    let field = |ty, index| value(ty, E::Field(Box::new(local(environment, 0)), index));
    let mut advance = function(
        vec![environment, Type::Int],
        Type::Int,
        Some(value(
            Type::Int,
            E::Binary(
                Binary::Add,
                Box::new(field(Type::Int, 0)),
                Box::new(primitive(
                    Type::Int,
                    Primitive::TextLen,
                    vec![field(Type::Text, 1)],
                )),
            ),
        )),
    );
    advance.body.statements.push(statement(S::Expr(value(
        Type::Unit,
        E::FrameStore {
            frame: Box::new(local(environment, 0)),
            field: 0,
            value: Box::new(value(
                Type::Int,
                E::Binary(
                    Binary::Add,
                    Box::new(field(Type::Int, 0)),
                    Box::new(local(Type::Int, 1)),
                ),
            )),
        },
    ))));
    let mut factory = function(
        vec![Type::Text],
        callable,
        Some(value(
            callable,
            E::Field(Box::new(local(environment, 1)), 2),
        )),
    );
    factory.locals.push(environment);
    factory.body.statements = vec![
        statement(S::Let {
            local: 1,
            value: value(
                environment,
                E::FrameNew(vec![(0, int(0)), (1, local(Type::Text, 0))]),
            ),
        }),
        // The closure refers back to the payload that stores it. Moving GC
        // must retain this cycle without tracing the entry as a data pointer.
        statement(S::Expr(value(
            Type::Unit,
            E::FrameStore {
                frame: Box::new(local(environment, 1)),
                field: 2,
                value: Box::new(value(
                    callable,
                    E::Closure {
                        function: 0,
                        environment: Box::new(local(environment, 1)),
                    },
                )),
            },
        ))),
    ];
    let mut main = function(vec![], Type::Unit, None);
    main.locals = vec![callable, wrapper, variant, list, callable];
    main.body.statements = vec![
        statement(S::Let {
            local: 0,
            value: value(
                callable,
                E::Call(
                    1,
                    vec![primitive(
                        Type::Text,
                        Primitive::TextConcat,
                        vec![text("se"), text("ed")],
                    )],
                ),
            ),
        }),
        statement(S::Let {
            local: 1,
            value: value(wrapper, E::Record(vec![(0, local(callable, 0))])),
        }),
        statement(S::Let {
            local: 2,
            value: value(
                variant,
                E::Variant {
                    variant: 0,
                    fields: vec![local(callable, 0)],
                },
            ),
        }),
        statement(S::Let {
            local: 3,
            value: value(list, E::List(vec![local(callable, 0)])),
        }),
        // Drop the original binding; each aggregate independently retains the
        // same mutable environment after its creator's native frame has gone.
        statement(S::Assign {
            local: 0,
            value: reference(0, 3),
        }),
        equal(
            invoke(
                Type::Int,
                value(callable, E::Field(Box::new(local(wrapper, 1)), 0)),
                vec![int(1)],
            ),
            int(5),
        ),
        equal(
            value(
                Type::Int,
                E::Match {
                    value: Box::new(local(variant, 2)),
                    arms: vec![checked::MatchArm {
                        variant: Some(0),
                        bindings: vec![Some(4)],
                        whole: None,
                        body: body(
                            vec![],
                            Some(invoke(Type::Int, local(callable, 4), vec![int(2)])),
                        ),
                    }],
                },
            ),
            int(7),
        ),
        equal(
            invoke(
                Type::Int,
                primitive(callable, Primitive::ListGet, vec![local(list, 3), int(0)]),
                vec![int(3)],
            ),
            int(10),
        ),
        equal(invoke(Type::Int, local(callable, 0), vec![int(0)]), int(99)),
    ];
    let alternate = function(vec![Type::Int], Type::Int, Some(int(99)));
    let unused = function(vec![environment, Type::Int], Type::Int, Some(int(9001)));
    let mut program = program(
        vec![advance, factory, main, alternate, unused],
        vec![checked::Signature {
            params: vec![Type::Int],
            result: Type::Int,
        }],
    );
    program.types = vec![
        checked::Data {
            name: "capture".into(),
            kind: checked::DataKind::Frame(vec![
                ("count".into(), Type::Int),
                ("label".into(), Type::Text),
                ("self".into(), callable),
            ]),
        },
        checked::Data {
            name: "wrapper".into(),
            kind: checked::DataKind::Record(vec![("call".into(), callable)]),
        },
        checked::Data {
            name: "variant".into(),
            kind: checked::DataKind::Enum(vec![("Some".into(), vec![callable])]),
        },
    ];
    program.lists.push(callable);
    for optimization in [Optimization::O0, Optimization::O2] {
        let (ir, output) = emit_run_at(&program, false, true, optimization);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!ir.contains("@loom.fn.4("));
    }
}

#[test]
fn captured_callee_snapshot_survives_allocating_argument_reassignment() {
    let callable = Type::Function(0);
    let environment = Type::Data(0);
    let read = value(Type::Text, E::Field(Box::new(local(environment, 0)), 0));
    let join = function(
        vec![environment, Type::Text],
        Type::Text,
        Some(primitive(
            Type::Text,
            Primitive::TextConcat,
            vec![read, local(Type::Text, 1)],
        )),
    );
    let factory = function(
        vec![Type::Text],
        callable,
        Some(value(
            callable,
            E::Closure {
                function: 0,
                environment: Box::new(value(
                    environment,
                    E::FrameNew(vec![(0, local(Type::Text, 0))]),
                )),
            },
        )),
    );
    let create = |label| {
        value(
            callable,
            E::Call(
                1,
                vec![primitive(
                    Type::Text,
                    Primitive::TextConcat,
                    vec![text(label), text("")],
                )],
            ),
        )
    };
    let mut main = function(vec![], Type::Unit, None);
    main.locals = vec![callable];
    main.body.statements = vec![
        statement(S::Let {
            local: 0,
            value: create("old"),
        }),
        equal(
            invoke(
                Type::Text,
                local(callable, 0),
                vec![value(
                    Type::Text,
                    E::Block(body(
                        vec![statement(S::Assign {
                            local: 0,
                            value: create("new"),
                        })],
                        Some(primitive(
                            Type::Text,
                            Primitive::TextConcat,
                            vec![text("+"), text("tail")],
                        )),
                    )),
                )],
            ),
            text("old+tail"),
        ),
        equal(
            invoke(Type::Text, local(callable, 0), vec![text("!")]),
            text("new!"),
        ),
    ];
    let mut program = program(
        vec![join, factory, main],
        vec![checked::Signature {
            params: vec![Type::Text],
            result: Type::Text,
        }],
    );
    program.types.push(checked::Data {
        name: "capture".into(),
        kind: checked::DataKind::Frame(vec![("label".into(), Type::Text)]),
    });
    for optimization in [Optimization::O0, Optimization::O2] {
        let (_, output) = emit_run_at(&program, false, true, optimization);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn assertion_locations_and_active_test_names_do_not_instrument_plain_entries() {
    let passed = function(vec![], Type::Unit, None);
    let mut helper = function(vec![], Type::Unit, None);
    helper.body.statements.push(statement(S::Assert {
        condition: value(Type::Bool, E::Bool(false)),
        message: "helpers.loom:4:5: assertion failed".into(),
    }));
    let mut failed = function(vec![], Type::Unit, None);
    failed
        .body
        .statements
        .push(statement(S::Expr(value(Type::Unit, E::Call(1, vec![])))));
    let mut program = program(vec![passed, helper, failed], vec![]);
    program.tests = vec![0, 2];
    program.test_names = vec!["sample.passes".into(), "sample.fails".into()];
    let (ir, output) = emit_run(&program, false, false);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        diagnostic_text(&output.stderr),
        "loom: helpers.loom:4:5: assertion failed\n"
    );
    assert!(!ir.contains("loom_rt_"));
    assert!(!ir.contains("sample.fails"));
    let (ir, output) = emit_run(&program, true, true);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        diagnostic_text(&output.stderr),
        "FAIL sample.fails\nRuntimeFault: helpers.loom:4:5: assertion failed\n"
    );
    assert!(ir.contains("loom_rt_test_enter"));
    assert!(ir.contains("loom_rt_test_leave"));
}

#[test]
fn named_callbacks_keep_direct_abi_dce_and_scalar_paths_runtime_free() {
    let callable = Type::Function(0);
    let mut increment = function(
        vec![Type::Int],
        Type::Int,
        Some(value(
            Type::Int,
            E::Binary(Binary::Add, Box::new(local(Type::Int, 0)), Box::new(int(1))),
        )),
    );
    increment.requires.push(value(
        Type::Bool,
        E::Binary(Binary::Ge, Box::new(local(Type::Int, 0)), Box::new(int(0))),
    ));
    let unused = function(vec![Type::Int], Type::Int, Some(int(9001)));
    let mut main = function(vec![], Type::Unit, None);
    main.locals.push(callable);
    let apply = function(
        vec![callable, Type::Int],
        Type::Int,
        Some(invoke(
            Type::Int,
            local(callable, 0),
            vec![local(Type::Int, 1)],
        )),
    );
    let mut unwrap = function(
        vec![Type::Data(1)],
        callable,
        Some(value(
            callable,
            E::Match {
                value: Box::new(local(Type::Data(1), 0)),
                arms: vec![checked::MatchArm {
                    variant: Some(0),
                    bindings: vec![Some(1)],
                    whole: None,
                    body: body(vec![], Some(local(callable, 1))),
                }],
            },
        )),
    );
    unwrap.locals.push(callable);
    let mut notify = function(vec![Type::Int], Type::Unit, None);
    notify.requires.push(value(
        Type::Bool,
        E::Binary(Binary::Eq, Box::new(local(Type::Int, 0)), Box::new(int(42))),
    ));
    let alternate = function(vec![Type::Int], Type::Int, Some(int(99)));
    let mut failure = function(vec![], Type::Unit, None);
    failure.body.statements.push(statement(S::Discard(invoke(
        Type::Int,
        reference(0, 0),
        vec![int(-1)],
    ))));
    main.body.statements = vec![
        equal(
            value(Type::Int, E::Call(3, vec![reference(0, 0), int(41)])),
            int(42),
        ),
        equal(
            invoke(
                Type::Int,
                value(
                    callable,
                    E::Field(
                        Box::new(value(Type::Data(0), E::Record(vec![(0, reference(0, 0))]))),
                        0,
                    ),
                ),
                vec![int(41)],
            ),
            int(42),
        ),
        equal(
            invoke(
                Type::Int,
                value(
                    callable,
                    E::Call(
                        4,
                        vec![value(
                            Type::Data(1),
                            E::Variant {
                                variant: 0,
                                fields: vec![reference(0, 0)],
                            },
                        )],
                    ),
                ),
                vec![int(41)],
            ),
            int(42),
        ),
        statement(S::Let {
            local: 0,
            value: reference(0, 0),
        }),
        equal(
            invoke(
                Type::Int,
                local(callable, 0),
                vec![value(
                    Type::Int,
                    E::Block(body(
                        vec![statement(S::Assign {
                            local: 0,
                            value: reference(0, 6),
                        })],
                        Some(int(41)),
                    )),
                )],
            ),
            int(42),
        ),
        statement(S::Expr(invoke(Type::Unit, reference(1, 5), vec![int(42)]))),
    ];
    let mut program = program(
        vec![
            increment, unused, main, apply, unwrap, notify, alternate, failure,
        ],
        vec![
            checked::Signature {
                params: vec![Type::Int],
                result: Type::Int,
            },
            checked::Signature {
                params: vec![Type::Int],
                result: Type::Unit,
            },
        ],
    );
    program.types = vec![
        checked::Data {
            name: "Callback".into(),
            kind: checked::DataKind::Record(vec![("action".into(), callable)]),
        },
        checked::Data {
            name: "Choice".into(),
            kind: checked::DataKind::Enum(vec![("Action".into(), vec![callable])]),
        },
    ];
    program.tests = vec![7];
    program.test_names = vec!["package.callback_failure".into()];
    program.exports = vec![1];
    assert_eq!(
        reachable_functions(&program, &[2], false)
            .unwrap()
            .functions,
        BTreeSet::from([0, 2, 3, 4, 5, 6])
    );
    assert!(gc::managed(&program, callable));
    assert_eq!(value_words(&program, Type::Data(1)).unwrap(), 3);
    let reachable = reachable_functions(&program, &[2], false)
        .unwrap()
        .functions;
    assert!(gc::allocating_functions(&program, &reachable, false).is_empty());
    assert!(gc::allocating_functions(&program, &reachable, true).contains(&3));
    let (ir, output) = emit_run(&program, false, false);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!ir.contains("@loom.fn.1("));
    assert!(!ir.contains("loom_rt_"));
    assert!(!ir.contains("package.callback_failure"));
    assert!(ir.contains("indirect.call"));
    let (ir, output) = emit_run(&program, true, true);
    assert!(ir.contains("loom_rt_test_enter"));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("precondition failed"));
    assert!(diagnostic_text(&output.stderr).starts_with("FAIL package.callback_failure\n"));
}

#[test]
fn allocating_callbacks_preserve_callee_and_argument_snapshots() {
    let callable = Type::Function(0);
    let concatenate = function(
        vec![Type::Text; 2],
        Type::Text,
        Some(primitive(
            Type::Text,
            Primitive::TextConcat,
            vec![local(Type::Text, 0), local(Type::Text, 1)],
        )),
    );
    let alternate = function(vec![Type::Text; 2], Type::Text, Some(text("wrong")));
    let mut main = function(vec![], Type::Unit, None);
    main.locals = vec![callable, Type::Text, Type::List(0)];
    let replacement = value(
        Type::Text,
        E::Block(body(
            vec![
                statement(S::Assign {
                    local: 0,
                    value: reference(0, 1),
                }),
                statement(S::Assign {
                    local: 1,
                    value: primitive(
                        Type::Text,
                        Primitive::TextConcat,
                        vec![text("new"), text("")],
                    ),
                }),
            ],
            Some(primitive(
                Type::Text,
                Primitive::TextConcat,
                vec![text("+"), text("tail")],
            )),
        )),
    );
    main.body.statements = vec![
        statement(S::Let {
            local: 0,
            value: reference(0, 0),
        }),
        statement(S::Let {
            local: 1,
            value: primitive(
                Type::Text,
                Primitive::TextConcat,
                vec![text("old"), text("")],
            ),
        }),
        equal(
            invoke(
                Type::Text,
                local(callable, 0),
                vec![local(Type::Text, 1), replacement],
            ),
            text("old+tail"),
        ),
        statement(S::Let {
            local: 2,
            value: primitive(Type::List(0), Primitive::ListNew, vec![]),
        }),
        statement(S::Expr(primitive(
            Type::Unit,
            Primitive::ListPush,
            vec![local(Type::List(0), 2), reference(0, 0)],
        ))),
        equal(
            invoke(
                Type::Text,
                primitive(
                    callable,
                    Primitive::ListGet,
                    vec![local(Type::List(0), 2), int(0)],
                ),
                vec![
                    primitive(
                        Type::Text,
                        Primitive::TextConcat,
                        vec![text("kept"), text("")],
                    ),
                    primitive(
                        Type::Text,
                        Primitive::TextConcat,
                        vec![text("+"), text("list")],
                    ),
                ],
            ),
            text("kept+list"),
        ),
    ];
    let mut program = program(
        vec![concatenate, alternate, main],
        vec![checked::Signature {
            params: vec![Type::Text; 2],
            result: Type::Text,
        }],
    );
    program.lists.push(callable);
    let (_, output) = emit_run(&program, false, true);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
