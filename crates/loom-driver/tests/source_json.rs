use std::fs;

use loom_core::{FileId, Span};
use loom_driver::{AnalysisHost, format_source};
use loom_interpreter::{Interpreter, Value};

fn invoke(interpreter: &mut Interpreter<'_>, parse: loom_mir::FunctionId, source: &str) -> Value {
    interpreter
        .invoke(
            parse,
            vec![Value::Text {
                value: source.to_owned(),
            }],
            Span::default(),
        )
        .expect("source JSON parser executes")
}

fn expect_ok(value: Value) -> Value {
    match value {
        Value::Enum {
            variant,
            mut payload,
            ..
        } if variant.0 == 0 && payload.len() == 1 => payload.remove(0),
        other => panic!("expected Ok, got {other:#?}"),
    }
}

fn expect_error(value: Value, expected_variant: u32, expected_offset: Option<i64>) {
    let error = match value {
        Value::Enum {
            variant,
            mut payload,
            ..
        } if variant.0 == 1 && payload.len() == 1 => payload.remove(0),
        other => panic!("expected Err, got {other:#?}"),
    };
    match (error, expected_offset) {
        (
            Value::Enum {
                variant, payload, ..
            },
            None,
        ) if variant.0 == expected_variant && payload.is_empty() => {}
        (
            Value::Enum {
                variant, payload, ..
            },
            Some(expected),
        ) if variant.0 == expected_variant && payload == vec![Value::Int { value: expected }] => {}
        (other, _) => panic!("unexpected JSON error {other:#?}"),
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one parser contract keeps direct source binding and the complete JSON edge corpus together"
)]
fn source_json_parser_handles_syntax_offsets_surrogates_numbers_and_depth() {
    let source = include_str!("../../../library/std/src/json.loom");
    let formatted = format_source(FileId(0), source);
    assert!(
        formatted.diagnostics.is_empty(),
        "{:#?}",
        formatted.diagnostics
    );
    assert_eq!(formatted.text, source, "std.json source must be canonical");

    let project = tempfile::tempdir().expect("temporary source JSON project");
    fs::write(
        project.path().join("main.loom"),
        "module source_json_test\n\nimport std.json.parse_json\n\nfn forward(text Text) Result[Json, JsonError] {\n    parse_json(text)\n}\n",
    )
    .expect("source JSON test application");
    let snapshot = AnalysisHost::new(project.path())
        .expect("source JSON test host")
        .snapshot()
        .expect("source JSON test snapshot");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("source JSON test MIR");
    let parse = program
        .functions
        .iter()
        .find(|function| function.name == "std.json.parse_json")
        .expect("source parse_json")
        .id;
    let forward = program
        .functions
        .iter()
        .find(|function| function.name == "source_json_test.forward")
        .expect("source JSON forwarding function");
    assert!(forward.exprs_preorder().any(|expression| {
        matches!(
            expression.kind,
            loom_mir::ExprKind::Call {
                target: loom_mir::CallTarget::Direct(target),
                ..
            } if target == parse
        )
    }));
    let mut interpreter = Interpreter::new(program);

    let text = expect_ok(invoke(
        &mut interpreter,
        parse,
        "\"line\\n\\u754c\\uD83D\\uDE42\"",
    ));
    assert!(matches!(
        text,
        Value::Enum {
            variant,
            payload,
            ..
        } if variant.0 == 3 && payload == vec![Value::Text { value: "line\n界🙂".to_owned() }]
    ));
    let escaped = expect_ok(invoke(&mut interpreter, parse, r#""\"\\\/\b\f\n\r\t""#));
    assert!(matches!(
        escaped,
        Value::Enum {
            variant,
            payload,
            ..
        } if variant.0 == 3
            && payload == vec![Value::Text { value: "\"\\/\u{8}\u{c}\n\r\t".to_owned() }]
    ));
    let utf8_boundaries = expect_ok(invoke(
        &mut interpreter,
        parse,
        r#""\u0000\u007F\u0080\u07FF\u0800\uFFFF\uD800\uDC00\uDBFF\uDFFF""#,
    ));
    assert!(matches!(
        utf8_boundaries,
        Value::Enum {
            variant,
            payload,
            ..
        } if variant.0 == 3
            && payload == vec![Value::Text {
                value: "\0\u{7f}\u{80}\u{7ff}\u{800}\u{ffff}\u{10000}\u{10ffff}".to_owned()
            }]
    ));
    let _ = expect_ok(invoke(
        &mut interpreter,
        parse,
        " {\"values\":[null,true,false,-0,1.25,2e3],\"界\":\"ok\"} ",
    ));
    let negative_zero = expect_ok(invoke(&mut interpreter, parse, "-0"));
    assert!(matches!(
        negative_zero,
        Value::Enum {
            variant,
            payload,
            ..
        } if variant.0 == 2
            && matches!(payload.as_slice(), [Value::Float { value }] if value.to_bits() == (-0.0_f64).to_bits())
    ));

    expect_error(invoke(&mut interpreter, parse, "\"\\uD800\""), 0, Some(2));
    expect_error(invoke(&mut interpreter, parse, "\"\\uDC00\""), 0, Some(2));
    expect_error(
        invoke(&mut interpreter, parse, "\"\\uD800\\u0041\""),
        0,
        Some(2),
    );
    expect_error(invoke(&mut interpreter, parse, "\"\\u12X4\""), 0, Some(3));
    expect_error(invoke(&mut interpreter, parse, "\"\\x\""), 0, Some(2));
    expect_error(invoke(&mut interpreter, parse, "\""), 0, Some(0));
    expect_error(
        invoke(&mut interpreter, parse, "\"line\nfeed\""),
        0,
        Some(5),
    );
    expect_error(
        invoke(&mut interpreter, parse, "{\"界\":1,\"界\":2}"),
        0,
        Some(9),
    );
    expect_error(
        invoke(&mut interpreter, parse, "{\"a\":1,\"\\u0061\":2}"),
        0,
        Some(7),
    );
    expect_error(
        invoke(&mut interpreter, parse, "{\"z\":0,\"z\":1,\"a\":2,\"a\":3}"),
        0,
        Some(19),
    );
    expect_error(
        invoke(&mut interpreter, parse, "{\"a\":0,\"a\":1,\"z\":2,\"z\":3}"),
        0,
        Some(7),
    );
    expect_error(invoke(&mut interpreter, parse, "1e999"), 1, Some(0));
    expect_error(invoke(&mut interpreter, parse, "01"), 0, Some(1));
    expect_error(invoke(&mut interpreter, parse, "-01"), 0, Some(2));
    expect_error(invoke(&mut interpreter, parse, "-"), 0, Some(1));
    expect_error(invoke(&mut interpreter, parse, "1."), 0, Some(2));
    expect_error(invoke(&mut interpreter, parse, "1e+"), 0, Some(3));
    expect_error(invoke(&mut interpreter, parse, "NaN"), 0, Some(0));
    expect_error(invoke(&mut interpreter, parse, "[1,]"), 0, Some(3));
    expect_error(invoke(&mut interpreter, parse, "[\"界\",]"), 0, Some(7));
    expect_error(invoke(&mut interpreter, parse, "{\"a\" 1}"), 0, Some(5));
    expect_error(invoke(&mut interpreter, parse, "true false"), 0, Some(5));
    expect_error(invoke(&mut interpreter, parse, " \n\t"), 0, Some(3));

    let flat_array = format!("[{}]", vec!["null"; 512].join(","));
    let _ = expect_ok(invoke(&mut interpreter, parse, &flat_array));
    let flat_object = format!(
        "{{{}}}",
        (0..128)
            .map(|index| format!("\"key{index}\":{index}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let _ = expect_ok(invoke(&mut interpreter, parse, &flat_object));
    let escaped_text = format!("\"{}\"", "\\u754c".repeat(512));
    let text = expect_ok(invoke(&mut interpreter, parse, &escaped_text));
    assert!(matches!(
        text,
        Value::Enum {
            variant,
            payload,
            ..
        } if variant.0 == 3
            && payload == vec![Value::Text { value: "界".repeat(512) }]
    ));

    let at_limit = format!("{}null{}", "[".repeat(128), "]".repeat(128));
    let beyond_limit = format!("{}null{}", "[".repeat(129), "]".repeat(129));
    let _ = expect_ok(invoke(&mut interpreter, parse, &at_limit));
    expect_error(invoke(&mut interpreter, parse, &beyond_limit), 2, None);

    let object_at_limit = format!("{}null{}", "{\"value\":".repeat(128), "}".repeat(128));
    let object_beyond_limit = format!("{}null{}", "{\"value\":".repeat(129), "}".repeat(129));
    let _ = expect_ok(invoke(&mut interpreter, parse, &object_at_limit));
    expect_error(
        invoke(&mut interpreter, parse, &object_beyond_limit),
        2,
        None,
    );
}
