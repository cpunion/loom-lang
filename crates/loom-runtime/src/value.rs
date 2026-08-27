//! Runtime helpers for the compiler-private uniform value representation.

use std::ffi::c_void;
use std::fmt::Write as _;

use loom_runtime_abi::{
    VALUE_TAG_BOOL, VALUE_TAG_CONSTRAINT_ERROR, VALUE_TAG_DYN, VALUE_TAG_ENUM, VALUE_TAG_FLOAT,
    VALUE_TAG_INT, VALUE_TAG_LIST, VALUE_TAG_RECORD, VALUE_TAG_REFINED, VALUE_TAG_TASK,
    VALUE_TAG_TEXT, VALUE_TAG_TUPLE, VALUE_TAG_UNIT,
};

use crate::scheduler::ValueSlot;
use crate::{WAIT_INVALID_ARGUMENT, WAIT_OK};

fn write_summary(value: &ValueSlot, output: &mut String) {
    match value.words[0] {
        VALUE_TAG_UNIT => output.push_str("Unit"),
        VALUE_TAG_BOOL => output.push_str("Bool"),
        VALUE_TAG_INT => output.push_str("Int"),
        VALUE_TAG_FLOAT => output.push_str("Float"),
        VALUE_TAG_TEXT => output.push_str("Text"),
        VALUE_TAG_RECORD | VALUE_TAG_ENUM | VALUE_TAG_REFINED => {
            let _ = write!(output, "type#{}", value.words[1]);
        }
        VALUE_TAG_CONSTRAINT_ERROR => output.push_str("ConstraintError"),
        VALUE_TAG_DYN => output.push_str("dyn"),
        VALUE_TAG_TASK => output.push_str("Task"),
        VALUE_TAG_TUPLE => output.push_str("Tuple"),
        VALUE_TAG_LIST => output.push_str("List"),
        _ => output.push_str("<invalid>"),
    }
}

/// Produces the same disclosure-safe summary used by the MIR interpreter.
#[unsafe(export_name = "loom_runtime_value_summary")]
pub unsafe extern "C" fn value_summary(value: *const c_void, output: *mut c_void) -> i32 {
    if value.is_null() || output.is_null() {
        return WAIT_INVALID_ARGUMENT;
    }
    let mut summary = String::new();
    // SAFETY: generated code passes aligned live Value slots.
    write_summary(unsafe { &*value.cast::<ValueSlot>() }, &mut summary);
    if summary.chars().count() > 256 {
        summary = summary.chars().take(253).collect::<String>() + "...";
    }
    let Some(result) = crate::gc::text_value(summary.as_bytes()) else {
        return WAIT_INVALID_ARGUMENT;
    };
    unsafe { output.cast::<ValueSlot>().write(result) };
    WAIT_OK
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_are_type_only_and_do_not_disclose_scalar_or_aggregate_data() {
        for (value, expected) in [
            (
                ValueSlot {
                    words: [VALUE_TAG_BOOL, 0, 0, 1, 0, 0],
                },
                "Bool",
            ),
            (
                ValueSlot {
                    words: [VALUE_TAG_INT, 0, 0, 0x5e_c0_e7_u64, 0, 0],
                },
                "Int",
            ),
            (
                ValueSlot {
                    words: [VALUE_TAG_FLOAT, 0, 0, 42.5_f64.to_bits(), 0, 0],
                },
                "Float",
            ),
            (
                ValueSlot {
                    words: [VALUE_TAG_TEXT, 0, 4_294_967_295, 0, 0, 0],
                },
                "Text",
            ),
            (
                ValueSlot {
                    words: [VALUE_TAG_LIST, 0, 9_876_543_210, 0, 0, 0],
                },
                "List",
            ),
            (
                ValueSlot {
                    words: [VALUE_TAG_ENUM, 27, 9_876_543_210, 0, 0, 0],
                },
                "type#27",
            ),
        ] {
            let mut summary = String::new();
            write_summary(&value, &mut summary);
            assert_eq!(summary, expected);
        }
    }
}
