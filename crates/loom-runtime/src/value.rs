//! Runtime helpers for the compiler-private uniform value representation.

use std::ffi::c_void;
use std::fmt::Write as _;

use crate::float::canonical_text;
use crate::gc::retain_bytes;
use crate::scheduler::{ValueNode, ValueSlot};
use crate::{WAIT_INVALID_ARGUMENT, WAIT_OK};

const VALUE_TAG_UNIT: u64 = 0;
const VALUE_TAG_BOOL: u64 = 1;
const VALUE_TAG_INT: u64 = 2;
const VALUE_TAG_FLOAT: u64 = 3;
const VALUE_TAG_TEXT: u64 = 4;
const VALUE_TAG_RECORD: u64 = 5;
const VALUE_TAG_ENUM: u64 = 6;
const VALUE_TAG_REFINED: u64 = 7;
const VALUE_TAG_CONSTRAINT_ERROR: u64 = 8;
const VALUE_TAG_DYN: u64 = 9;
const VALUE_TAG_TUPLE: u64 = 10;
const VALUE_TAG_TASK: u64 = 11;
const VALUE_TAG_LIST: u64 = 12;

fn write_nodes(mut node: *const ValueNode, count: u64, output: &mut String, depth: u8) {
    for index in 0..count {
        if index > 0 {
            output.push_str(", ");
        }
        if node.is_null() {
            output.push_str("<invalid>");
            return;
        }
        // SAFETY: native aggregate counts and chains are created by checked
        // generated code and remain live throughout this helper call.
        unsafe {
            write_summary(&(*node).value, output, depth);
            node = (*node).next;
        }
    }
}

fn write_summary(value: &ValueSlot, output: &mut String, depth: u8) {
    if depth >= 6 {
        output.push_str("...");
        return;
    }
    match value.words[0] {
        VALUE_TAG_UNIT => output.push_str("Unit"),
        VALUE_TAG_BOOL => output.push_str(if value.words[3] == 0 { "false" } else { "true" }),
        VALUE_TAG_INT => {
            let _ = write!(output, "{}", value.words[3].cast_signed());
        }
        VALUE_TAG_FLOAT => output.push_str(&canonical_text(f64::from_bits(value.words[3]))),
        VALUE_TAG_TEXT => {
            let _ = write!(output, "Text(bytes={})", value.words[2]);
        }
        VALUE_TAG_RECORD => {
            let _ = write!(output, "type#{}", value.words[1]);
        }
        VALUE_TAG_ENUM => {
            let _ = write!(
                output,
                "type#{}::variant#{}",
                value.words[1], value.words[2]
            );
        }
        VALUE_TAG_REFINED => {
            let _ = write!(output, "type#{}(", value.words[1]);
            let inner = value.words[4] as *const ValueSlot;
            if inner.is_null() {
                output.push_str("<invalid>");
            } else {
                // SAFETY: refined payloads are runtime-managed Value pointers.
                write_summary(unsafe { &*inner }, output, depth + 1);
            }
            output.push(')');
        }
        VALUE_TAG_CONSTRAINT_ERROR => output.push_str("ConstraintError"),
        VALUE_TAG_DYN => output.push_str("<dyn interface>"),
        VALUE_TAG_TASK => output.push_str("<task>"),
        VALUE_TAG_TUPLE => {
            output.push('(');
            write_nodes(
                value.words[4] as *const ValueNode,
                value.words[2],
                output,
                depth + 1,
            );
            if value.words[2] == 1 {
                output.push(',');
            }
            output.push(')');
        }
        VALUE_TAG_LIST => {
            output.push('[');
            write_nodes(
                value.words[4] as *const ValueNode,
                value.words[2],
                output,
                depth + 1,
            );
            output.push(']');
        }
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
    write_summary(unsafe { &*value.cast::<ValueSlot>() }, &mut summary, 0);
    if summary.chars().count() > 256 {
        summary = summary.chars().take(253).collect::<String>() + "...";
    }
    let (data, length) = retain_bytes(summary.into_bytes());
    let mut result = ValueSlot::default();
    result.words[0] = VALUE_TAG_TEXT;
    result.words[2] = length;
    result.words[4] = data as u64;
    unsafe { output.cast::<ValueSlot>().write(result) };
    WAIT_OK
}
