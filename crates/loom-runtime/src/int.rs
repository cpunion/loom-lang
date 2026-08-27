//! Integer/text boundary for compiler-known standard-library operations.

use std::num::IntErrorKind;
use std::slice;

use loom_runtime_abi::{PARSE_STATUS_INVALID_SYNTAX, PARSE_STATUS_OK, PARSE_STATUS_OUT_OF_RANGE};

#[unsafe(export_name = "loom_runtime_parse_int")]
pub unsafe extern "C" fn parse_int(data: *const u8, length: u64, output: *mut i64) -> i32 {
    if data.is_null() || output.is_null() {
        return PARSE_STATUS_INVALID_SYNTAX;
    }
    let Ok(length) = usize::try_from(length) else {
        return PARSE_STATUS_OUT_OF_RANGE;
    };
    // SAFETY: generated Text supplies a readable byte range of its stored length.
    let Ok(text) = std::str::from_utf8(unsafe { slice::from_raw_parts(data, length) }) else {
        return PARSE_STATUS_INVALID_SYNTAX;
    };
    match text.parse::<i64>() {
        Ok(value) => {
            // SAFETY: output was checked non-null.
            unsafe { *output = value };
            PARSE_STATUS_OK
        }
        Err(error) => match error.kind() {
            IntErrorKind::PosOverflow | IntErrorKind::NegOverflow => PARSE_STATUS_OUT_OF_RANGE,
            _ => PARSE_STATUS_INVALID_SYNTAX,
        },
    }
}

#[cfg(test)]
mod tests {
    use loom_runtime_abi::{
        PARSE_STATUS_INVALID_SYNTAX, PARSE_STATUS_OK, PARSE_STATUS_OUT_OF_RANGE,
    };

    fn parse(text: &str, output: &mut i64) -> i32 {
        // SAFETY: the test supplies a live UTF-8 slice and writable scalar cell.
        unsafe {
            super::parse_int(
                text.as_ptr(),
                u64::try_from(text.len()).expect("test input length"),
                output,
            )
        }
    }

    #[test]
    fn scalar_boundary_uses_the_shared_closed_status_contract() {
        let mut output = 0;
        assert_eq!(parse("-17", &mut output), PARSE_STATUS_OK);
        assert_eq!(output, -17);
        assert_eq!(parse("17x", &mut output), PARSE_STATUS_INVALID_SYNTAX);
        assert_eq!(
            parse("9223372036854775808", &mut output),
            PARSE_STATUS_OUT_OF_RANGE
        );
    }
}
