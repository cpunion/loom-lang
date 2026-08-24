#![no_main]

use libfuzzer_sys::fuzz_target;
use loom_core::{FileId, Span};
use loom_hir::{SourceUnit, lower_files};
use loom_interpreter::{Interpreter, Value};
use loom_lowering::lower_to_mir;
use loom_sema::analyze;
use loom_syntax::parse_with_file;

fuzz_target!(|input: &[u8]| {
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
         pub fn main() Unit {{\n\
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
    let program = compile(&source).unwrap_or_else(|error| {
        panic!("generated proof program must compile:\n{source}\n{error}")
    });
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

fn bounded(input: &[u8], start: usize) -> i64 {
    let mut bytes = [0_u8; 8];
    for (index, slot) in bytes.iter_mut().enumerate() {
        *slot = input.get(start + index).copied().unwrap_or_default();
    }
    i64::from_le_bytes(bytes).rem_euclid(1001)
}

fn compile(source: &str) -> Result<loom_mir::Program, String> {
    let parsed = parse_with_file(FileId(0), source);
    if !parsed.diagnostics().is_empty() {
        return Err(format!("syntax diagnostics: {:#?}", parsed.diagnostics()));
    }
    let lowered = lower_files([SourceUnit {
        file: FileId(0),
        syntax: parsed.ast(),
    }]);
    if !lowered.diagnostics.is_empty() {
        return Err(format!("HIR diagnostics: {:#?}", lowered.diagnostics));
    }
    let analysis = analyze(&lowered.program);
    if !analysis.diagnostics.is_empty() {
        return Err(format!(
            "semantic diagnostics: {:#?}",
            analysis.diagnostics
        ));
    }
    let program = lower_to_mir(&lowered.program, &analysis)
        .map_err(|failure| format!("MIR lowering diagnostics: {:#?}", failure.diagnostics()))?;
    program
        .validate()
        .map_err(|errors| format!("MIR validation diagnostics: {errors:#?}"))?;
    Ok(program)
}
