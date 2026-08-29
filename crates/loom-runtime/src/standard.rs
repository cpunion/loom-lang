//! Runtime primitives for immutable `Text`, `Bytes`, and lexical `Path`.
//!
//! Native `Bytes` and `Path` are nominal records whose private payload uses a
//! managed immutable sequence object. Text and arbitrary Bytes have distinct
//! descriptors; valid UTF-8 storage may be shared only where the type-level
//! operation preserves the Text invariant.

use std::collections::BTreeMap;
use std::ffi::c_void;

use loom_runtime_abi::{
    GC_OK, VALUE_TAG_BOOL, VALUE_TAG_ENUM, VALUE_TAG_FLOAT, VALUE_TAG_INT, VALUE_TAG_LIST,
    VALUE_TAG_RECORD, VALUE_TAG_TEXT,
};

use crate::gc::{NodeStream, RuntimeRootScope};
use crate::scheduler::{ValueNode, ValueSlot};
use crate::{gc, text, write_process_stderr};

const STANDARD_INVALID_ARGUMENT: i32 = -1;
pub const JSON_DEPTH_LIMIT: usize = 128;

#[derive(Clone, Debug, PartialEq)]
pub enum JsonNode {
    Null,
    Bool(bool),
    Number(f64),
    Text(String),
    Array(Vec<JsonNode>),
    Object(BTreeMap<String, JsonNode>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JsonFormatFailure {
    DepthLimit,
    NonFiniteNumber,
}

/// Formats a JSON value using Loom's canonical ordering and escaping rules.
///
/// # Errors
///
/// Returns `DepthLimit` for values nested beyond 128 containers and
/// `NonFiniteNumber` for NaN or infinite numbers.
pub fn format_json(value: &JsonNode) -> Result<String, JsonFormatFailure> {
    let mut output = String::new();
    format_json_value(value, 0, &mut output)?;
    Ok(output)
}

/// Escapes one Text value as a JSON string, including the surrounding quotes.
pub fn escape_json_text(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(value.len() + 2);
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{08}' => output.push_str("\\b"),
            '\u{0c}' => output.push_str("\\f"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            control if control <= '\u{1f}' => {
                let value = u32::from(control) as usize;
                output.push_str("\\u00");
                output.push(char::from(HEX[value >> 4]));
                output.push(char::from(HEX[value & 0x0f]));
            }
            character => output.push(character),
        }
    }
    output.push('"');
    output
}

fn format_json_value(
    value: &JsonNode,
    depth: usize,
    output: &mut String,
) -> Result<(), JsonFormatFailure> {
    match value {
        JsonNode::Null => output.push_str("null"),
        JsonNode::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        JsonNode::Number(value) => {
            if !value.is_finite() {
                return Err(JsonFormatFailure::NonFiniteNumber);
            }
            output.push_str(&value.to_string());
        }
        JsonNode::Text(value) => output.push_str(&escape_json_text(value)),
        JsonNode::Array(values) => {
            if depth >= JSON_DEPTH_LIMIT {
                return Err(JsonFormatFailure::DepthLimit);
            }
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                format_json_value(value, depth + 1, output)?;
            }
            output.push(']');
        }
        JsonNode::Object(values) => {
            if depth >= JSON_DEPTH_LIMIT {
                return Err(JsonFormatFailure::DepthLimit);
            }
            output.push('{');
            for (index, (key, value)) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                output.push_str(&escape_json_text(key));
                output.push(':');
                format_json_value(value, depth + 1, output)?;
            }
            output.push('}');
        }
    }
    Ok(())
}

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

fn store_text(output: *mut c_void, bytes: &[u8]) -> i32 {
    if output.is_null() {
        return STANDARD_INVALID_ARGUMENT;
    }
    let Some(value) = gc::text_value(bytes) else {
        return STANDARD_INVALID_ARGUMENT;
    };
    // SAFETY: generated code supplies an aligned writable ValueSlot.
    unsafe { output.cast::<ValueSlot>().write(value) };
    0
}

fn store_bytes(output: *mut c_void, bytes: &[u8]) -> i32 {
    if output.is_null() {
        return STANDARD_INVALID_ARGUMENT;
    }
    let Some(value) = gc::byte_value(bytes) else {
        return STANDARD_INVALID_ARGUMENT;
    };
    // SAFETY: generated code supplies an aligned writable ValueSlot.
    unsafe { output.cast::<ValueSlot>().write(value) };
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
    if store_text(output, &encoded) == 0 {
        1
    } else {
        -1
    }
}

unsafe fn concatenate(
    left: *const c_void,
    left_length: u64,
    right: *const c_void,
    right_length: u64,
) -> Option<Vec<u8>> {
    let left = unsafe { input_bytes(left, left_length) }?;
    let right = unsafe { input_bytes(right, right_length) }?;
    let capacity = left.len().checked_add(right.len())?;
    let mut value = Vec::with_capacity(capacity);
    value.extend_from_slice(left);
    value.extend_from_slice(right);
    Some(value)
}

/// Concatenates two valid Text payloads into a new managed Text object.
#[unsafe(export_name = "loom_runtime_text_concat")]
pub unsafe extern "C" fn text_concat(
    left: *const c_void,
    left_length: u64,
    right: *const c_void,
    right_length: u64,
    output: *mut c_void,
) -> i32 {
    let Some(value) = (unsafe { concatenate(left, left_length, right, right_length) }) else {
        return STANDARD_INVALID_ARGUMENT;
    };
    store_text(output, &value)
}

/// Concatenates two arbitrary immutable byte sequences into a new managed
/// `ByteObject`. Invalid UTF-8 remains valid Bytes and cannot become Text.
#[unsafe(export_name = "loom_runtime_bytes_append")]
pub unsafe extern "C" fn bytes_append(
    left: *const c_void,
    left_length: u64,
    right: *const c_void,
    right_length: u64,
    output: *mut c_void,
) -> i32 {
    let Some(value) = (unsafe { concatenate(left, left_length, right, right_length) }) else {
        return STANDARD_INVALID_ARGUMENT;
    };
    store_bytes(output, &value)
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

/// Validates arbitrary Bytes and writes a distinct managed Text object on
/// success. Returns `1` for valid UTF-8, `0` for invalid UTF-8, and `-1` for
/// invalid ABI input.
#[unsafe(export_name = "loom_runtime_bytes_decode_utf8")]
pub unsafe extern "C" fn bytes_decode_utf8(
    data: *const c_void,
    length: u64,
    output: *mut c_void,
) -> i32 {
    if output.is_null() {
        return STANDARD_INVALID_ARGUMENT;
    }
    let Some(bytes) = (unsafe { input_bytes(data, length) }) else {
        return STANDARD_INVALID_ARGUMENT;
    };
    if std::str::from_utf8(bytes).is_err() {
        return 0;
    }
    if store_text(output, bytes) == 0 {
        1
    } else {
        STANDARD_INVALID_ARGUMENT
    }
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
    store_text(output, &value)
}

unsafe fn map_entries(map: *const ValueSlot) -> Option<Vec<(ValueSlot, ValueSlot)>> {
    if map.is_null()
        || unsafe { (*map).words[0] } != VALUE_TAG_RECORD
        || unsafe { (*map).words[2] } % 2 != 0
    {
        return None;
    }
    let count = usize::try_from(unsafe { (*map).words[2] } / 2).ok()?;
    let mut node = unsafe { (*map).words[4] as *const ValueNode };
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        if node.is_null() || unsafe { (*node).value.words[0] } != VALUE_TAG_TEXT {
            return None;
        }
        let key = unsafe { (*node).value };
        node = unsafe { (*node).next };
        if node.is_null() {
            return None;
        }
        let value = unsafe { (*node).value };
        node = unsafe { (*node).next };
        entries.push((key, value));
    }
    Some(entries)
}

unsafe fn text_slot_bytes(value: &ValueSlot) -> Option<&[u8]> {
    unsafe { text::text_value_bytes(value) }
}

fn build_aggregate(mut aggregate: ValueSlot, values: Vec<ValueSlot>) -> ValueSlot {
    let count_word = if aggregate.words[0] == VALUE_TAG_ENUM {
        3
    } else {
        2
    };
    aggregate.words[count_word] = 0;
    aggregate.words[4] = 0;
    if values.is_empty() {
        return aggregate;
    }
    let mut initial = Vec::with_capacity(values.len() + 1);
    initial.push(aggregate);
    initial.extend(values);
    let roots = RuntimeRootScope::from_values(initial).unwrap_or_else(|_| std::process::abort());
    let stream = NodeStream::new(&roots, 0, aggregate);
    for index in (1..roots.len()).rev() {
        if stream.prepend(index) != GC_OK {
            std::process::abort();
        }
    }
    roots.read(0)
}

fn build_map(nominal: u64, entries: Vec<(ValueSlot, ValueSlot)>) -> ValueSlot {
    let mut map = ValueSlot::default();
    map.words[0] = VALUE_TAG_RECORD;
    map.words[1] = nominal;
    let values = entries
        .into_iter()
        .flat_map(|(key, value)| [key, value])
        .collect();
    build_aggregate(map, values)
}

fn key_matches(slot: &ValueSlot, key: &[u8]) -> bool {
    // SAFETY: map keys were validated as Text slots before this helper.
    unsafe { text_slot_bytes(slot) }.is_some_and(|candidate| candidate == key)
}

#[unsafe(export_name = "loom_runtime_text_map_get")]
/// Returns `1` and copies the mapped value into stable caller storage, `0`
/// when absent, or `-1` for invalid ABI input.
pub unsafe extern "C" fn text_map_get(
    map: *const c_void,
    key: *const c_void,
    key_length: u64,
    output: *mut c_void,
) -> i32 {
    if output.is_null() {
        return STANDARD_INVALID_ARGUMENT;
    }
    let Some(key) = (unsafe { input_bytes(key, key_length) }) else {
        return STANDARD_INVALID_ARGUMENT;
    };
    let map = map.cast::<ValueSlot>();
    if map.is_null()
        || unsafe { (*map).words[0] } != VALUE_TAG_RECORD
        || unsafe { (*map).words[2] } % 2 != 0
    {
        return STANDARD_INVALID_ARGUMENT;
    }
    let mut node = unsafe { (*map).words[4] as *const ValueNode };
    for _ in 0..unsafe { (*map).words[2] / 2 } {
        if node.is_null() || !key_matches(unsafe { &(*node).value }, key) {
            if node.is_null() {
                return STANDARD_INVALID_ARGUMENT;
            }
            node = unsafe { (*node).next };
            if node.is_null() {
                return STANDARD_INVALID_ARGUMENT;
            }
            node = unsafe { (*node).next };
            continue;
        }
        let value = unsafe { (*node).next };
        if value.is_null() {
            return STANDARD_INVALID_ARGUMENT;
        }
        unsafe { output.cast::<ValueSlot>().write((*value).value) };
        return 1;
    }
    0
}

#[unsafe(export_name = "loom_runtime_text_map_insert")]
pub unsafe extern "C" fn text_map_insert(
    map: *const c_void,
    key: *const c_void,
    value: *const c_void,
    output: *mut c_void,
) -> i32 {
    let map = map.cast::<ValueSlot>();
    let key = key.cast::<ValueSlot>();
    let value = value.cast::<ValueSlot>();
    if key.is_null() || value.is_null() || output.is_null() {
        return STANDARD_INVALID_ARGUMENT;
    }
    let Some(mut entries) = (unsafe { map_entries(map) }) else {
        return STANDARD_INVALID_ARGUMENT;
    };
    let Some(key_bytes) = (unsafe { text_slot_bytes(&*key) }) else {
        return STANDARD_INVALID_ARGUMENT;
    };
    if let Some((_, existing)) = entries
        .iter_mut()
        .find(|(candidate, _)| key_matches(candidate, key_bytes))
    {
        *existing = unsafe { *value };
    } else {
        entries.push((unsafe { *key }, unsafe { *value }));
    }
    entries.sort_by(|left, right| {
        let left = unsafe { text_slot_bytes(&left.0) }.unwrap_or_default();
        let right = unsafe { text_slot_bytes(&right.0) }.unwrap_or_default();
        left.cmp(right)
    });
    let nominal = unsafe { (*map).words[1] };
    let result = build_map(nominal, entries);
    unsafe { output.cast::<ValueSlot>().write(result) };
    0
}

#[unsafe(export_name = "loom_runtime_text_map_remove")]
pub unsafe extern "C" fn text_map_remove(
    map: *const c_void,
    key: *const c_void,
    key_length: u64,
    output: *mut c_void,
) -> i32 {
    let map = map.cast::<ValueSlot>();
    if output.is_null() {
        return STANDARD_INVALID_ARGUMENT;
    }
    let Some(key) = (unsafe { input_bytes(key, key_length) }) else {
        return STANDARD_INVALID_ARGUMENT;
    };
    let Some(mut entries) = (unsafe { map_entries(map) }) else {
        return STANDARD_INVALID_ARGUMENT;
    };
    entries.retain(|(candidate, _)| !key_matches(candidate, key));
    let nominal = unsafe { (*map).words[1] };
    let result = build_map(nominal, entries);
    unsafe { output.cast::<ValueSlot>().write(result) };
    0
}

fn text_value(value: &str) -> ValueSlot {
    gc::text_value(value.as_bytes()).unwrap_or_else(|| std::process::abort())
}

fn enum_value(nominal: u64, variant: u64, payload: Vec<ValueSlot>) -> ValueSlot {
    let mut value = ValueSlot::default();
    value.words[0] = VALUE_TAG_ENUM;
    value.words[1] = nominal;
    value.words[2] = variant;
    build_aggregate(value, payload)
}

fn result_value(nominal: u64, ok: bool, payload: ValueSlot) -> ValueSlot {
    enum_value(nominal, u64::from(!ok), vec![payload])
}

unsafe fn node_values(mut node: *const ValueNode, count: u64) -> Option<Vec<ValueSlot>> {
    let count = usize::try_from(count).ok()?;
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        if node.is_null() {
            return None;
        }
        values.push(unsafe { (*node).value });
        node = unsafe { (*node).next };
    }
    Some(values)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SlotJsonFailure {
    InvalidShape,
    DepthLimit,
}

unsafe fn slot_json(
    value: &ValueSlot,
    json_type: u64,
    text_map_type: u64,
    depth: usize,
) -> Result<JsonNode, SlotJsonFailure> {
    if value.words[0] != VALUE_TAG_ENUM || value.words[1] != json_type {
        return Err(SlotJsonFailure::InvalidShape);
    }
    let payload = unsafe { node_values(value.words[4] as *const ValueNode, value.words[3]) }
        .ok_or(SlotJsonFailure::InvalidShape)?;
    match (value.words[2], payload.as_slice()) {
        (0, []) => Ok(JsonNode::Null),
        (1, [value]) if value.words[0] == VALUE_TAG_BOOL => Ok(JsonNode::Bool(value.words[3] != 0)),
        (2, [value]) if value.words[0] == VALUE_TAG_FLOAT => {
            Ok(JsonNode::Number(f64::from_bits(value.words[3])))
        }
        (3, [value]) => {
            let bytes = unsafe { text_slot_bytes(value) }.ok_or(SlotJsonFailure::InvalidShape)?;
            let text = std::str::from_utf8(bytes).map_err(|_| SlotJsonFailure::InvalidShape)?;
            Ok(JsonNode::Text(text.to_owned()))
        }
        (4, [list]) if list.words[0] == VALUE_TAG_LIST => {
            if depth >= JSON_DEPTH_LIMIT {
                return Err(SlotJsonFailure::DepthLimit);
            }
            let values = unsafe { node_values(list.words[4] as *const ValueNode, list.words[2]) }
                .ok_or(SlotJsonFailure::InvalidShape)?;
            Ok(JsonNode::Array(
                values
                    .iter()
                    .map(|value| unsafe { slot_json(value, json_type, text_map_type, depth + 1) })
                    .collect::<Result<Vec<_>, _>>()?,
            ))
        }
        (5, [map]) if map.words[0] == VALUE_TAG_RECORD && map.words[1] == text_map_type => {
            if depth >= JSON_DEPTH_LIMIT {
                return Err(SlotJsonFailure::DepthLimit);
            }
            let entries = unsafe { map_entries(map) }.ok_or(SlotJsonFailure::InvalidShape)?;
            let mut object = BTreeMap::new();
            for (key, value) in entries {
                let key = std::str::from_utf8(
                    unsafe { text_slot_bytes(&key) }.ok_or(SlotJsonFailure::InvalidShape)?,
                )
                .map_err(|_| SlotJsonFailure::InvalidShape)?
                .to_owned();
                object.insert(key, unsafe {
                    slot_json(&value, json_type, text_map_type, depth + 1)
                }?);
            }
            Ok(JsonNode::Object(object))
        }
        _ => Err(SlotJsonFailure::InvalidShape),
    }
}

fn json_error_slot(error: JsonFormatFailure, json_error_type: u64) -> ValueSlot {
    let variant = match error {
        JsonFormatFailure::DepthLimit => 2,
        JsonFormatFailure::NonFiniteNumber => 3,
    };
    enum_value(json_error_type, variant, Vec::new())
}

#[unsafe(export_name = "loom_runtime_json_format")]
pub unsafe extern "C" fn json_format(
    value: *const c_void,
    result_type: u64,
    json_type: u64,
    json_error_type: u64,
    text_map_type: u64,
    output: *mut c_void,
) -> i32 {
    if value.is_null() || output.is_null() {
        return STANDARD_INVALID_ARGUMENT;
    }
    let value = match unsafe { slot_json(&*value.cast::<ValueSlot>(), json_type, text_map_type, 0) }
    {
        Ok(value) => value,
        Err(SlotJsonFailure::DepthLimit) => {
            let result = result_value(
                result_type,
                false,
                json_error_slot(JsonFormatFailure::DepthLimit, json_error_type),
            );
            unsafe { output.cast::<ValueSlot>().write(result) };
            return 0;
        }
        Err(SlotJsonFailure::InvalidShape) => return STANDARD_INVALID_ARGUMENT,
    };
    let result = match format_json(&value) {
        Ok(value) => result_value(result_type, true, text_value(&value)),
        Err(error) => result_value(result_type, false, json_error_slot(error, json_error_type)),
    };
    unsafe { output.cast::<ValueSlot>().write(result) };
    0
}

#[unsafe(export_name = "loom_runtime_log")]
pub unsafe extern "C" fn log_write(
    level: u32,
    message: *const c_void,
    message_length: u64,
    fields: *const c_void,
) -> i32 {
    let Some(level) = ["debug", "info", "warn", "error"].get(level as usize) else {
        return STANDARD_INVALID_ARGUMENT;
    };
    let Some(message) = (unsafe { input_bytes(message, message_length) }) else {
        return STANDARD_INVALID_ARGUMENT;
    };
    let Ok(message) = std::str::from_utf8(message) else {
        return STANDARD_INVALID_ARGUMENT;
    };
    let entries = if fields.is_null() {
        Vec::new()
    } else {
        let Some(entries) = (unsafe { map_entries(fields.cast::<ValueSlot>()) }) else {
            return STANDARD_INVALID_ARGUMENT;
        };
        entries
    };
    let mut line = format!(
        "{{\"level\":{},\"message\":{},\"fields\":{{",
        escape_json_text(level),
        escape_json_text(message)
    );
    for (index, (key, value)) in entries.iter().enumerate() {
        let Some(key) = (unsafe { text_slot_bytes(key) }) else {
            return STANDARD_INVALID_ARGUMENT;
        };
        let Some(value) = (unsafe { text_slot_bytes(value) }) else {
            return STANDARD_INVALID_ARGUMENT;
        };
        let (Ok(key), Ok(value)) = (std::str::from_utf8(key), std::str::from_utf8(value)) else {
            return STANDARD_INVALID_ARGUMENT;
        };
        if index > 0 {
            line.push(',');
        }
        line.push_str(&escape_json_text(key));
        line.push(':');
        line.push_str(&escape_json_text(value));
    }
    line.push_str("}}\n");
    i32::from(write_process_stderr(line.as_bytes()) != loom_runtime_abi::TYPED_LOG_OK)
}

#[cfg(test)]
mod tests {
    use std::ptr;

    use super::*;

    struct ActiveRuntime(*mut crate::runtime::LoomRuntime);

    impl ActiveRuntime {
        fn new() -> Self {
            let runtime = crate::runtime::runtime_create_v1();
            assert!(!runtime.is_null());
            assert_eq!(
                unsafe { crate::gc::activate_runtime_v1(runtime) },
                crate::GC_OK
            );
            Self(runtime)
        }
    }

    impl Drop for ActiveRuntime {
        fn drop(&mut self) {
            assert_eq!(
                unsafe { crate::gc::deactivate_runtime_v1(self.0) },
                crate::GC_OK,
            );
            assert_eq!(
                unsafe { crate::runtime::runtime_destroy_v1(self.0) },
                crate::GC_OK,
            );
        }
    }

    fn text_parts(value: &ValueSlot) -> (&[u8], u64) {
        // SAFETY: test values remain live for the assertion.
        let bytes = unsafe { text::text_value_bytes(value) }.unwrap();
        (bytes, bytes.len() as u64)
    }

    #[test]
    fn managed_text_and_bytes_outputs_keep_descriptor_direction() {
        let _runtime = ActiveRuntime::new();
        let mut concatenated = ValueSlot::default();
        let mut arbitrary = ValueSlot::default();
        let mut decoded = ValueSlot::default();
        // SAFETY: all inputs and output slots remain live for each ABI call.
        unsafe {
            assert_eq!(
                text_concat(
                    b"a".as_ptr().cast(),
                    1,
                    "界".as_ptr().cast(),
                    3,
                    (&raw mut concatenated).cast(),
                ),
                0,
            );
            assert_eq!(
                text::text_value_bytes(&concatenated),
                Some("a界".as_bytes())
            );
            assert_eq!(text::byte_value_bytes(&concatenated), None);

            assert_eq!(
                bytes_append(
                    [0xff].as_ptr().cast(),
                    1,
                    [0].as_ptr().cast(),
                    1,
                    (&raw mut arbitrary).cast(),
                ),
                0,
            );
            assert_eq!(text::byte_value_bytes(&arbitrary), Some(&[0xff, 0][..]));
            assert_eq!(text::text_value_bytes(&arbitrary), None);
            assert_eq!(
                bytes_decode_utf8(b"ok".as_ptr().cast(), 2, (&raw mut decoded).cast(),),
                1,
            );
            assert_eq!(text::text_value_bytes(&decoded), Some(&b"ok"[..]));
            assert_eq!(
                bytes_decode_utf8([0xff].as_ptr().cast(), 1, (&raw mut decoded).cast(),),
                0,
            );
        }
    }

    #[test]
    fn managed_sequence_and_map_builders_survive_every_allocator_collection() {
        let runtime = ActiveRuntime::new();
        let roots = RuntimeRootScope::with_count(7).expect("runtime root scope");
        unsafe {
            (*runtime.0).heap.collect_before_every_allocation = true;

            roots.write(0, gc::byte_value("moving 界🙂".as_bytes()).unwrap());
            let source = roots.read(0);
            let source_bytes = text::value_bytes(&source).unwrap();
            assert_eq!(
                bytes_decode_utf8(
                    source_bytes.as_ptr().cast(),
                    source_bytes.len() as u64,
                    roots.pointer(1).cast(),
                ),
                1,
            );
            assert_eq!(
                text::text_value_bytes(&roots.read(1)),
                Some("moving 界🙂".as_bytes()),
            );

            roots.write(2, build_map(15, Vec::new()));
            roots.write(3, text_value("key"));
            roots.write(4, text_value("managed value"));
            assert_eq!(
                text_map_insert(
                    roots.pointer(2).cast(),
                    roots.pointer(3).cast(),
                    roots.pointer(4).cast(),
                    roots.pointer(5).cast(),
                ),
                0,
            );
            assert_eq!(
                text_map_get(
                    roots.pointer(5).cast(),
                    b"key".as_ptr().cast(),
                    3,
                    roots.pointer(6).cast(),
                ),
                1,
            );
            assert_eq!(
                text::text_value_bytes(&roots.read(6)),
                Some(&b"managed value"[..]),
            );
            let mapped_address = roots.read(6).words[loom_runtime_abi::VALUE_WORD_DATA];
            let _trigger = gc::text_value(b"trigger").expect("managed Text");
            assert_ne!(
                roots.read(6).words[loom_runtime_abi::VALUE_WORD_DATA],
                mapped_address,
            );
            assert_eq!(
                text::text_value_bytes(&roots.read(6)),
                Some(&b"managed value"[..]),
            );

            assert_eq!(
                text_map_remove(
                    roots.pointer(5).cast(),
                    b"key".as_ptr().cast(),
                    3,
                    roots.pointer(2).cast(),
                ),
                0,
            );
            assert_eq!(roots.read(2).words[2], 0);
            assert!((*runtime.0).heap.collections >= 7);
            (*runtime.0).heap.collect_before_every_allocation = false;
        }
        drop(roots);
        drop(runtime);
    }

    #[test]
    fn value_slot_json_format_survives_every_allocator_collection() {
        let runtime = ActiveRuntime::new();
        let roots = RuntimeRootScope::with_count(4).expect("runtime root scope");
        roots.write(0, text_value("key"));
        roots.write(1, text_value("value"));
        roots.write(2, enum_value(16, 3, vec![roots.read(1)]));
        roots.write(3, build_map(15, vec![(roots.read(0), roots.read(2))]));
        roots.write(0, enum_value(16, 5, vec![roots.read(3)]));
        unsafe {
            (*runtime.0).heap.collect_before_every_allocation = true;
            assert_eq!(
                json_format(
                    roots.pointer(0).cast(),
                    1,
                    16,
                    17,
                    15,
                    roots.pointer(1).cast(),
                ),
                0,
            );
            let formatted_result = roots.read(1);
            let formatted = node_values(
                formatted_result.words[4] as *const ValueNode,
                formatted_result.words[3],
            )
            .unwrap();
            assert_eq!(
                text::text_value_bytes(&formatted[0]),
                Some(&b"{\"key\":\"value\"}"[..]),
            );
            assert!((*runtime.0).heap.collections >= 2);
            (*runtime.0).heap.collect_before_every_allocation = false;
        }
        drop(roots);
        drop(runtime);
    }

    #[test]
    fn text_get_stages_a_scalar_before_allocator_collection() {
        let runtime = ActiveRuntime::new();
        let roots = RuntimeRootScope::with_count(2).expect("runtime root scope");
        unsafe {
            (*runtime.0).heap.collect_before_every_allocation = true;
            roots.write(0, gc::text_value("a界🙂".as_bytes()).unwrap());
            let source_address = roots.read(0).words[loom_runtime_abi::VALUE_WORD_DATA];
            let source = roots.read(0);
            let source_bytes = text::value_bytes(&source).unwrap();
            assert_eq!(
                text_get(
                    source_bytes.as_ptr().cast(),
                    source_bytes.len() as u64,
                    1,
                    roots.pointer(1).cast(),
                ),
                1,
            );
            assert_ne!(
                roots.read(0).words[loom_runtime_abi::VALUE_WORD_DATA],
                source_address,
            );
            assert_eq!(
                text::text_value_bytes(&roots.read(1)),
                Some("界".as_bytes())
            );
            let result_address = roots.read(1).words[loom_runtime_abi::VALUE_WORD_DATA];
            let _trigger = gc::text_value(b"trigger").expect("managed Text");
            assert_ne!(
                roots.read(1).words[loom_runtime_abi::VALUE_WORD_DATA],
                result_address,
            );
            assert_eq!(
                text::text_value_bytes(&roots.read(1)),
                Some("界".as_bytes())
            );
            (*runtime.0).heap.collect_before_every_allocation = false;
        }
        drop(roots);
        drop(runtime);
    }

    #[test]
    fn text_concat_stages_an_unrooted_typed_text_before_forced_collection() {
        let runtime = ActiveRuntime::new();
        let roots = RuntimeRootScope::with_count(1).expect("runtime root scope");
        unsafe {
            let units = [65_i64, 0xe7, 0x95, 0x8c, 0xf0, 0x9f, 0x99, 0x82];
            let mut typed = ptr::null_mut();
            assert_eq!(
                text::from_utf8_units_typed_v1(units.as_ptr(), units.len() as u64, &raw mut typed,),
                GC_OK,
            );
            let typed_bytes = text::text_bytes(typed).expect("typed Text bytes");

            // The compiler's checked-MIR bridge deliberately does not expose
            // a typed pointer through a universal Value root. Text.concat
            // must consume the complete borrowed range before this forced
            // allocation collection reclaims the temporary typed object.
            (*runtime.0).heap.collect_before_every_allocation = true;
            assert_eq!(
                text_concat(
                    typed_bytes.as_ptr().cast(),
                    typed_bytes.len() as u64,
                    ptr::null(),
                    0,
                    roots.pointer(0).cast(),
                ),
                GC_OK,
            );
            assert_eq!(
                text::text_value_bytes(&roots.read(0)),
                Some("A界🙂".as_bytes()),
            );
            assert_eq!((*runtime.0).heap.typed_object_count(), 0);
            assert!((*runtime.0).heap.collections >= 1);
            (*runtime.0).heap.collect_before_every_allocation = false;
        }
        drop(roots);
        drop(runtime);
    }

    #[test]
    fn unicode_bytes_and_portable_paths_are_distinct() {
        let _runtime = ActiveRuntime::new();
        let text = "a界🙂";
        let mut scalar = ValueSlot::default();
        // SAFETY: test buffers and outputs remain live for each call.
        unsafe {
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

    #[test]
    fn json_format_is_canonical_and_bounded() {
        let mut object = BTreeMap::new();
        object.insert("z".to_owned(), JsonNode::Number(-0.0));
        object.insert("a".to_owned(), JsonNode::Text("line\n".to_owned()));
        assert_eq!(
            format_json(&JsonNode::Object(object)).unwrap(),
            "{\"a\":\"line\\n\",\"z\":-0}",
        );
        assert_eq!(
            format_json(&JsonNode::Number(f64::INFINITY)).unwrap_err(),
            JsonFormatFailure::NonFiniteNumber,
        );

        let mut nested = JsonNode::Null;
        for _ in 0..128 {
            nested = JsonNode::Array(vec![nested]);
        }
        assert!(format_json(&nested).is_ok());
        nested = JsonNode::Array(vec![nested]);
        assert_eq!(
            format_json(&nested).unwrap_err(),
            JsonFormatFailure::DepthLimit,
        );
    }

    #[test]
    fn native_map_and_json_format_abi_preserve_shapes_and_depth_errors() {
        let _runtime = ActiveRuntime::new();
        let empty = build_map(15, Vec::new());
        let key = text_value("key");
        let mut value = ValueSlot::default();
        value.words[0] = VALUE_TAG_INT;
        value.words[3] = 42;
        let deep_root = RuntimeRootScope::with_count(1).expect("deep JSON root");
        deep_root.write(0, enum_value(16, 0, Vec::new()));
        for _ in 0..129 {
            let mut list = ValueSlot::default();
            list.words[0] = VALUE_TAG_LIST;
            let list = build_aggregate(list, vec![deep_root.read(0)]);
            deep_root.write(0, enum_value(16, 4, vec![list]));
        }
        let mut inserted = ValueSlot::default();
        // SAFETY: all slots and their runtime-owned payloads remain live for this test.
        unsafe {
            assert_eq!(
                text_map_insert(
                    (&raw const empty).cast(),
                    (&raw const key).cast(),
                    (&raw const value).cast(),
                    (&raw mut inserted).cast(),
                ),
                0,
            );
            let mut found = ValueSlot::default();
            assert_eq!(
                text_map_get(
                    (&raw const inserted).cast(),
                    b"key".as_ptr().cast(),
                    3,
                    (&raw mut found).cast(),
                ),
                1,
            );
            assert_eq!(found.words[0], VALUE_TAG_INT);
            assert_eq!(found.words[3], 42);

            let mut removed = ValueSlot::default();
            assert_eq!(
                text_map_remove(
                    (&raw const inserted).cast(),
                    b"key".as_ptr().cast(),
                    3,
                    (&raw mut removed).cast(),
                ),
                0,
            );
            assert_eq!(removed.words[2], 0);

            let mut formatted = ValueSlot::default();
            assert_eq!(
                json_format(
                    deep_root.pointer(0).cast(),
                    1,
                    16,
                    17,
                    15,
                    (&raw mut formatted).cast(),
                ),
                0,
            );
            assert_eq!(formatted.words[2], 1);
            let error = node_values(formatted.words[4] as *const ValueNode, 1).unwrap()[0];
            assert_eq!(error.words[2], 2);
        }
    }
}
