#![no_main]

mod support;

use libfuzzer_sys::fuzz_target;
use loom_core::Span;
use loom_interpreter::{Interpreter, Value};

use support::{STRUCTURED_STANDARD_SOURCE, compile};

fuzz_target!(|input: &[u8]| {
    if input.first().copied().unwrap_or_default() & 0x80 != 0 {
        exercise_structured_standard_values(input);
        return;
    }

    let candidate = bounded(input, 0);
    let first = bounded(input, 8);
    let second = bounded(input, 16);
    let kind = input.first().copied().unwrap_or_default() % 4;
    let (predicate, proven, rejected, accepted) = match kind {
        0 => (
            format!("self >= {first}"),
            first,
            first - 1,
            candidate >= first,
        ),
        1 => (
            format!("self <= {first}"),
            first,
            first + 1,
            candidate <= first,
        ),
        2 => (
            format!("self == {first}"),
            first,
            first + 1,
            candidate == first,
        ),
        _ => {
            let low = first.min(second);
            let high = first.max(second);
            (
                format!("self >= {low} && self <= {high}"),
                low,
                high + 1,
                candidate >= low && candidate <= high,
            )
        }
    };

    let source = format!(
        "module fuzz.proof\n\n\
         type Checked = Int where {predicate}\n\n\
         fn direct() Checked {{\n\
             Checked({proven})\n\
         }}\n\n\
         fn checked(value Int) Result[Checked, ConstraintError] {{\n\
             Checked(value)\n\
         }}\n\n\
         pub fn main() {{\n\
             let value = direct()\n\
             assert value == {proven}\n\
             match checked({candidate}) {{\n\
                 Ok(_) => {{\n\
                     assert {accepted}\n\
                     Unit\n\
                 }}\n\
                 Err(_) => {{\n\
                     assert {}\n\
                     Unit\n\
                 }}\n\
             }}\n\
         }}\n",
        !accepted
    );
    let program = compile(&source)
        .unwrap_or_else(|error| panic!("generated proof program must compile:\n{source}\n{error}"));
    let main = *program.exports.get("main").expect("main export");
    let result = Interpreter::new(&program)
        .invoke(main, Vec::new(), Span::default())
        .expect("static proof and runtime constraint classification must agree");
    assert_eq!(result, Value::Unit);

    // A literal that contradicts the same predicate must never be silently
    // accepted as a direct constrained value.
    let rejected_source = format!(
        "module fuzz.reject\n\n\
         type Checked = Int where {predicate}\n\n\
         fn rejected() Checked {{ Checked({rejected}) }}\n"
    );
    assert!(compile(&rejected_source).is_err());
});

fn exercise_structured_standard_values(input: &[u8]) {
    let program = compile(STRUCTURED_STANDARD_SOURCE).unwrap_or_else(|error| {
        panic!("generated structured standard-value program must compile: {error}")
    });
    assert!(program.prelude.text_map.is_some());
    assert!(program.prelude.json.is_some());
    assert!(program.prelude.json_error.is_some());
    assert!(program.prelude.io_error.is_some());
    assert!(program.prelude.io_error_kind.is_some());
    assert!(program.prelude.log_level.is_some());

    let invalid = match input.get(1).copied().unwrap_or_default() % 6 {
        0 => {
            r#"
module fuzz.structured.reject
fn bad[V](left TextMap[V], right TextMap[V]) Bool { left == right }
"#
        }
        1 => {
            r#"
module fuzz.structured.reject
fn bad() TextMap[Int] { TextMap[Int]().insert("key", "wrong") }
"#
        }
        2 => {
            r#"
module fuzz.structured.reject
fn bad(value Json) {
    match value {
        Null => Unit
        Bool(_) => Unit
    }
}
"#
        }
        3 => {
            r#"
module fuzz.structured.reject
fn bad() Json { Json.Null() }
"#
        }
        4 => {
            r#"
module fuzz.structured.reject
fn bad(value IoErrorKind) {
    match value {
        NotFound => Unit
        Other => Unit
    }
}
"#
        }
        _ => {
            r#"
module fuzz.structured.reject
import standard.log.write
fn bad() {
    write(LogLevel.Info, "event", TextMap[Int]())
}
"#
        }
    };
    assert!(
        compile(invalid).is_err(),
        "structured semantic mutation must fail:\n{invalid}"
    );
}

fn bounded(input: &[u8], start: usize) -> i64 {
    let mut bytes = [0_u8; 8];
    for (index, slot) in bytes.iter_mut().enumerate() {
        *slot = input.get(start + index).copied().unwrap_or_default();
    }
    i64::from_le_bytes(bytes).rem_euclid(1001)
}
