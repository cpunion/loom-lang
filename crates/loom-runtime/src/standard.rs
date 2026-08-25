//! Runtime primitives for immutable `Text`, `Bytes`, and lexical `Path`.
//!
//! Native `Bytes` and `Path` are nominal records whose private payload uses
//! the same pointer/length slot as `Text`. The nominal wrapper keeps the types
//! distinct while sharing immutable storage and the moving byte arena.

use std::ffi::c_void;

use crate::gc::retain_bytes;
use crate::scheduler::ValueSlot;

const VALUE_TAG_INT: u64 = 2;
const VALUE_TAG_TEXT: u64 = 4;
const STANDARD_INVALID_ARGUMENT: i32 = -1;

unsafe fn input_bytes<'value>(data: *const c_void, length: u64) -> Option<&'value [u8]> {
    let length = usize::try_from(length).ok()?;
    if length == 0 {
        return Some(&[]);
    }
    if data.is_null() {
        return None;
    }
    // SAFETY: generated code supplies a live immutable buffer and its exact
    // length. The returned slice is used only during the current ABI call.
    Some(unsafe { std::slice::from_raw_parts(data.cast::<u8>(), length) })
}

fn store_text(output: *mut c_void, bytes: Vec<u8>) -> i32 {
    if output.is_null() {
        return STANDARD_INVALID_ARGUMENT;
    }
    let (data, length) = retain_bytes(bytes);
    let mut value = ValueSlot::default();
    value.words[0] = VALUE_TAG_TEXT;
    value.words[2] = length;
    value.words[4] = data as u64;
    // SAFETY: generated code supplies an aligned writable ValueSlot.
    unsafe { output.cast::<ValueSlot>().write(value) };
    0
}

/// Counts Unicode scalar values, not UTF-8 code units. Returns `-1` for an
/// invalid pointer or invalid UTF-8; checked `Text` never takes that path.
#[unsafe(export_name = "loom_runtime_text_length")]
pub unsafe extern "C" fn text_length(data: *const c_void, length: u64, output: *mut i64) -> i32 {
    if output.is_null() {
        return STANDARD_INVALID_ARGUMENT;
    }
    let Some(bytes) = (unsafe { input_bytes(data, length) }) else {
        return STANDARD_INVALID_ARGUMENT;
    };
    let Ok(text) = std::str::from_utf8(bytes) else {
        return STANDARD_INVALID_ARGUMENT;
    };
    let Ok(length) = i64::try_from(text.chars().count()) else {
        return STANDARD_INVALID_ARGUMENT;
    };
    // SAFETY: output was checked non-null above.
    unsafe { output.write(length) };
    0
}

/// Returns `1` and writes a one-scalar Text when found, `0` when out of
/// bounds, or `-1` for an invalid ABI input.
#[unsafe(export_name = "loom_runtime_text_get")]
pub unsafe extern "C" fn text_get(
    data: *const c_void,
    length: u64,
    index: i64,
    output: *mut c_void,
) -> i32 {
    if output.is_null() {
        return STANDARD_INVALID_ARGUMENT;
    }
    let Some(bytes) = (unsafe { input_bytes(data, length) }) else {
        return STANDARD_INVALID_ARGUMENT;
    };
    let Ok(text) = std::str::from_utf8(bytes) else {
        return STANDARD_INVALID_ARGUMENT;
    };
    let Some(index) = usize::try_from(index).ok() else {
        return 0;
    };
    let Some(scalar) = text.chars().nth(index) else {
        return 0;
    };
    let mut encoded = [0_u8; 4];
    let encoded = scalar.encode_utf8(&mut encoded).as_bytes().to_vec();
    if store_text(output, encoded) == 0 {
        1
    } else {
        -1
    }
}

/// Concatenates two immutable byte sequences into a new runtime-owned Text
/// payload. The same operation backs Text concat and Bytes append.
#[unsafe(export_name = "loom_runtime_bytes_append")]
pub unsafe extern "C" fn bytes_append(
    left: *const c_void,
    left_length: u64,
    right: *const c_void,
    right_length: u64,
    output: *mut c_void,
) -> i32 {
    let Some(left) = (unsafe { input_bytes(left, left_length) }) else {
        return STANDARD_INVALID_ARGUMENT;
    };
    let Some(right) = (unsafe { input_bytes(right, right_length) }) else {
        return STANDARD_INVALID_ARGUMENT;
    };
    let Some(capacity) = left.len().checked_add(right.len()) else {
        return STANDARD_INVALID_ARGUMENT;
    };
    let mut value = Vec::with_capacity(capacity);
    value.extend_from_slice(left);
    value.extend_from_slice(right);
    store_text(output, value)
}

/// Byte-subsequence containment is equivalent to Text substring containment
/// for valid UTF-8 and does not need allocation.
#[unsafe(export_name = "loom_runtime_text_contains")]
pub unsafe extern "C" fn text_contains(
    value: *const c_void,
    value_length: u64,
    needle: *const c_void,
    needle_length: u64,
) -> i32 {
    let Some(value) = (unsafe { input_bytes(value, value_length) }) else {
        return STANDARD_INVALID_ARGUMENT;
    };
    let Some(needle) = (unsafe { input_bytes(needle, needle_length) }) else {
        return STANDARD_INVALID_ARGUMENT;
    };
    if needle.is_empty() {
        return 1;
    }
    i32::from(value.windows(needle.len()).any(|window| window == needle))
}

/// Returns `1` and writes an Int byte value, `0` when out of bounds, or `-1`
/// for an invalid ABI input.
#[unsafe(export_name = "loom_runtime_bytes_get")]
pub unsafe extern "C" fn bytes_get(
    data: *const c_void,
    length: u64,
    index: i64,
    output: *mut c_void,
) -> i32 {
    if output.is_null() {
        return STANDARD_INVALID_ARGUMENT;
    }
    let Some(bytes) = (unsafe { input_bytes(data, length) }) else {
        return STANDARD_INVALID_ARGUMENT;
    };
    let Some(value) = usize::try_from(index)
        .ok()
        .and_then(|index| bytes.get(index))
        .copied()
    else {
        return 0;
    };
    let mut result = ValueSlot::default();
    result.words[0] = VALUE_TAG_INT;
    result.words[3] = u64::from(value);
    // SAFETY: output was checked non-null above.
    unsafe { output.cast::<ValueSlot>().write(result) };
    1
}

/// `1` means valid UTF-8, `0` means invalid UTF-8, and `-1` is an invalid ABI
/// pointer. This is intentionally distinct from Text validation at the type
/// boundary because arbitrary Bytes are permitted.
#[unsafe(export_name = "loom_runtime_bytes_is_utf8")]
pub unsafe extern "C" fn bytes_is_utf8(data: *const c_void, length: u64) -> i32 {
    let Some(bytes) = (unsafe { input_bytes(data, length) }) else {
        return STANDARD_INVALID_ARGUMENT;
    };
    i32::from(std::str::from_utf8(bytes).is_ok())
}

#[unsafe(export_name = "loom_runtime_path_contains_nul")]
pub unsafe extern "C" fn path_contains_nul(data: *const c_void, length: u64) -> i32 {
    let Some(bytes) = (unsafe { input_bytes(data, length) }) else {
        return STANDARD_INVALID_ARGUMENT;
    };
    i32::from(bytes.contains(&0))
}

/// Portable lexical join. Only a leading `/` is absolute; drive letters and
/// backslashes have no special meaning in Loom's platform-independent Path.
/// Returns `0` on success, `1` for an absolute child, and `-1` for invalid ABI
/// input.
#[unsafe(export_name = "loom_runtime_path_join")]
pub unsafe extern "C" fn path_join(
    base: *const c_void,
    base_length: u64,
    child: *const c_void,
    child_length: u64,
    output: *mut c_void,
) -> i32 {
    let Some(base) = (unsafe { input_bytes(base, base_length) }) else {
        return STANDARD_INVALID_ARGUMENT;
    };
    let Some(child) = (unsafe { input_bytes(child, child_length) }) else {
        return STANDARD_INVALID_ARGUMENT;
    };
    if child.first() == Some(&b'/') {
        return 1;
    }
    let separator = usize::from(!base.is_empty() && !base.ends_with(b"/") && !child.is_empty());
    let Some(capacity) = base
        .len()
        .checked_add(separator)
        .and_then(|length| length.checked_add(child.len()))
    else {
        return STANDARD_INVALID_ARGUMENT;
    };
    let mut value = Vec::with_capacity(capacity);
    value.extend_from_slice(base);
    if separator != 0 {
        value.push(b'/');
    }
    value.extend_from_slice(child);
    store_text(output, value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;

    fn text_parts(value: &ValueSlot) -> (&[u8], u64) {
        let length = value.words[2];
        // SAFETY: ValueSlot contains the runtime-owned pointer/length pair.
        let bytes = unsafe { input_bytes(value.words[4] as *const c_void, length) }.unwrap();
        (bytes, length)
    }

    #[test]
    fn unicode_bytes_and_portable_paths_are_distinct() {
        let text = "a界🙂";
        let mut scalar_count = 0;
        let mut scalar = ValueSlot::default();
        // SAFETY: test buffers and outputs remain live for each call.
        unsafe {
            assert_eq!(
                text_length(
                    text.as_ptr().cast(),
                    text.len() as u64,
                    &raw mut scalar_count,
                ),
                0
            );
            assert_eq!(scalar_count, 3);
            assert_eq!(
                text_get(
                    text.as_ptr().cast(),
                    text.len() as u64,
                    1,
                    (&raw mut scalar).cast(),
                ),
                1
            );
            assert_eq!(text_parts(&scalar).0, "界".as_bytes());

            assert_eq!(bytes_is_utf8([0xff].as_ptr().cast(), 1), 0);
            let mut joined = ValueSlot::default();
            assert_eq!(
                path_join(
                    b"base".as_ptr().cast(),
                    4,
                    b"child".as_ptr().cast(),
                    5,
                    (&raw mut joined).cast(),
                ),
                0
            );
            assert_eq!(text_parts(&joined).0, b"base/child");
            assert_eq!(
                path_join(
                    b"base".as_ptr().cast(),
                    4,
                    b"/child".as_ptr().cast(),
                    6,
                    ptr::null_mut(),
                ),
                1
            );
        }
    }
}
