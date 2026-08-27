//! Canonical binary64 text boundary used by compiler-generated builtins.

use std::ffi::{c_char, c_int};

use loom_runtime_abi::{PARSE_STATUS_INVALID_SYNTAX, PARSE_STATUS_OK, PARSE_STATUS_OUT_OF_RANGE};

use crate::scheduler::ValueSlot;

const CANONICAL_NAN: u64 = 0x7ff8_0000_0000_0000;

fn has_float_syntax(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut index = usize::from(bytes.first() == Some(&b'-'));
    let integer_start = index;
    while bytes.get(index).is_some_and(u8::is_ascii_digit) {
        index += 1;
    }
    if index == integer_start {
        return false;
    }

    let mut decimal = false;
    if bytes.get(index) == Some(&b'.') {
        decimal = true;
        index += 1;
        let fraction_start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if index == fraction_start {
            return false;
        }
    }

    let mut exponent = false;
    if matches!(bytes.get(index), Some(b'e' | b'E')) {
        exponent = true;
        index += 1;
        if matches!(bytes.get(index), Some(b'+' | b'-')) {
            index += 1;
        }
        let exponent_start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if index == exponent_start {
            return false;
        }
    }
    index == bytes.len() && (decimal || exponent)
}

pub(crate) fn canonical_text(value: f64) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    if value == f64::INFINITY {
        return "Infinity".into();
    }
    if value == f64::NEG_INFINITY {
        return "-Infinity".into();
    }
    let mut text = value.to_string();
    if !text.contains(['.', 'e', 'E']) {
        text.push_str(".0");
    }
    text
}

/// Parses Loom's canonical float syntax.
///
/// Returns 0 on success, 1 for invalid syntax and 2 for finite-range overflow.
#[unsafe(export_name = "loom_runtime_parse_float")]
pub unsafe extern "C" fn parse_float(data: *const c_char, length: u64, output: *mut f64) -> c_int {
    let Ok(length) = usize::try_from(length) else {
        return PARSE_STATUS_INVALID_SYNTAX;
    };
    if data.is_null() || output.is_null() || length > isize::MAX as usize {
        return PARSE_STATUS_INVALID_SYNTAX;
    }
    // SAFETY: the compiler passes a live Text payload and its checked length.
    let bytes = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), length) };
    let Ok(text) = std::str::from_utf8(bytes) else {
        return PARSE_STATUS_INVALID_SYNTAX;
    };
    let value = match text {
        "NaN" => f64::from_bits(CANONICAL_NAN),
        "Infinity" => f64::INFINITY,
        "-Infinity" => f64::NEG_INFINITY,
        _ if has_float_syntax(text) => {
            let Ok(value) = text.parse::<f64>() else {
                return PARSE_STATUS_INVALID_SYNTAX;
            };
            if value.is_infinite() {
                return PARSE_STATUS_OUT_OF_RANGE;
            }
            value
        }
        _ => return PARSE_STATUS_INVALID_SYNTAX,
    };
    // SAFETY: output was checked non-null and is an LLVM stack slot for f64.
    unsafe { output.write(value) };
    PARSE_STATUS_OK
}

/// Formats a binary64 value into one managed Text object.
#[unsafe(export_name = "loom_runtime_format_float")]
pub unsafe extern "C" fn format_float(value: f64, output: *mut ValueSlot) -> c_int {
    if output.is_null() {
        return 1;
    }
    let text = canonical_text(value);
    let Some(result) = crate::gc::text_value(text.as_bytes()) else {
        return 1;
    };
    // SAFETY: the caller-owned stable Value slot was checked non-null.
    unsafe { output.write(result) };
    0
}

#[cfg(test)]
mod tests {
    use loom_runtime_abi::{
        PARSE_STATUS_INVALID_SYNTAX, PARSE_STATUS_OK, PARSE_STATUS_OUT_OF_RANGE,
    };

    use super::{canonical_text, has_float_syntax};

    fn parse(text: &str, output: &mut f64) -> i32 {
        // SAFETY: the test supplies a live UTF-8 slice and writable scalar cell.
        unsafe {
            super::parse_float(
                text.as_ptr().cast(),
                u64::try_from(text.len()).expect("test input length"),
                output,
            )
        }
    }

    #[test]
    fn scalar_boundary_uses_the_shared_closed_status_contract() {
        let mut output = 1.0;
        assert_eq!(parse("-0.0", &mut output), PARSE_STATUS_OK);
        assert_eq!(output.to_bits(), (-0.0_f64).to_bits());
        assert_eq!(parse("1", &mut output), PARSE_STATUS_INVALID_SYNTAX);
        assert_eq!(parse("1e999", &mut output), PARSE_STATUS_OUT_OF_RANGE);
    }

    #[test]
    fn text_boundary_matches_language_contract() {
        assert!(has_float_syntax("1.0"));
        assert!(has_float_syntax("1e3"));
        assert!(!has_float_syntax("1"));
        assert_eq!(canonical_text(1e20), "100000000000000000000.0");
        assert_eq!(canonical_text(1e-7), "0.0000001");
        assert_eq!(canonical_text(-0.0), "-0.0");
        assert_eq!(canonical_text(f64::INFINITY), "Infinity");
        assert_eq!(canonical_text(f64::NAN), "NaN");
    }

    #[test]
    fn format_writes_a_stable_value_slot_before_the_next_allocation() {
        let runtime = crate::runtime::runtime_create_v1();
        assert!(!runtime.is_null());
        unsafe {
            assert_eq!(crate::gc::activate_runtime_v1(runtime), crate::GC_OK);
            let roots = crate::gc::RuntimeRootScope::with_count(1).expect("runtime root scope");
            (*runtime).heap.collect_before_every_allocation = true;
            assert_eq!(super::format_float(12.5, roots.pointer(0)), 0);
            let first_address = roots.read(0).words[loom_runtime_abi::VALUE_WORD_DATA];
            let _trigger = crate::gc::text_value(b"trigger").expect("managed Text");
            assert_ne!(
                roots.read(0).words[loom_runtime_abi::VALUE_WORD_DATA],
                first_address,
            );
            assert_eq!(
                crate::text::text_value_bytes(&roots.read(0)),
                Some(&b"12.5"[..]),
            );
            (*runtime).heap.collect_before_every_allocation = false;
            drop(roots);
            assert_eq!(crate::gc::deactivate_runtime_v1(runtime), crate::GC_OK);
            assert_eq!(crate::runtime::runtime_destroy_v1(runtime), crate::WAIT_OK,);
        }
    }
}
