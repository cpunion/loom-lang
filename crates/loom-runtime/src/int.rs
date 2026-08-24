//! Integer/text boundary for compiler-known standard-library operations.

use std::num::IntErrorKind;
use std::slice;

pub const PARSE_INT_OK: i32 = 0;
pub const PARSE_INT_INVALID_SYNTAX: i32 = 1;
pub const PARSE_INT_OUT_OF_RANGE: i32 = 2;

#[unsafe(export_name = "loom_runtime_parse_int")]
pub unsafe extern "C" fn parse_int(data: *const u8, length: u64, output: *mut i64) -> i32 {
    if data.is_null() || output.is_null() {
        return PARSE_INT_INVALID_SYNTAX;
    }
    let Ok(length) = usize::try_from(length) else {
        return PARSE_INT_OUT_OF_RANGE;
    };
    // SAFETY: generated Text supplies a readable byte range of its stored length.
    let Ok(text) = std::str::from_utf8(unsafe { slice::from_raw_parts(data, length) }) else {
        return PARSE_INT_INVALID_SYNTAX;
    };
    match text.parse::<i64>() {
        Ok(value) => {
            // SAFETY: output was checked non-null.
            unsafe { *output = value };
            PARSE_INT_OK
        }
        Err(error) => match error.kind() {
            IntErrorKind::PosOverflow | IntErrorKind::NegOverflow => PARSE_INT_OUT_OF_RANGE,
            _ => PARSE_INT_INVALID_SYNTAX,
        },
    }
}
