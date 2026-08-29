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
pub enum JsonFailureKind {
    InvalidSyntax,
    NumberOutOfRange,
    DepthLimit,
    NonFiniteNumber,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JsonFailure {
    pub kind: JsonFailureKind,
    pub offset: usize,
}

/// Parses one complete UTF-8 JSON document with deterministic byte offsets.
///
/// # Errors
///
/// Returns the stable syntax, numeric-range, or depth-limit failure and its
/// zero-based UTF-8 byte offset.
pub fn parse_json(source: &str) -> Result<JsonNode, JsonFailure> {
    JsonParser {
        source,
        bytes: source.as_bytes(),
        index: 0,
    }
    .parse()
}

/// Formats a JSON value using Loom's canonical ordering and escaping rules.
///
/// # Errors
///
/// Returns `DepthLimit` for values nested beyond 128 containers and
/// `NonFiniteNumber` for NaN or infinite numbers.
pub fn format_json(value: &JsonNode) -> Result<String, JsonFailure> {
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

struct JsonParser<'source> {
    source: &'source str,
    bytes: &'source [u8],
    index: usize,
}

impl JsonParser<'_> {
    fn parse(mut self) -> Result<JsonNode, JsonFailure> {
        self.skip_whitespace();
        let value = self.parse_value(0)?;
        self.skip_whitespace();
        if self.index == self.bytes.len() {
            Ok(value)
        } else {
            Err(Self::failure(JsonFailureKind::InvalidSyntax, self.index))
        }
    }

    fn parse_value(&mut self, depth: usize) -> Result<JsonNode, JsonFailure> {
        let Some(byte) = self.bytes.get(self.index).copied() else {
            return Err(Self::failure(JsonFailureKind::InvalidSyntax, self.index));
        };
        match byte {
            b'n' => {
                self.consume_keyword(b"null")?;
                Ok(JsonNode::Null)
            }
            b't' => {
                self.consume_keyword(b"true")?;
                Ok(JsonNode::Bool(true))
            }
            b'f' => {
                self.consume_keyword(b"false")?;
                Ok(JsonNode::Bool(false))
            }
            b'"' => self.parse_string().map(JsonNode::Text),
            b'[' => self.parse_array(depth),
            b'{' => self.parse_object(depth),
            b'-' | b'0'..=b'9' => self.parse_number(),
            _ => Err(Self::failure(JsonFailureKind::InvalidSyntax, self.index)),
        }
    }

    fn parse_array(&mut self, depth: usize) -> Result<JsonNode, JsonFailure> {
        if depth >= JSON_DEPTH_LIMIT {
            return Err(Self::failure(JsonFailureKind::DepthLimit, self.index));
        }
        self.index += 1;
        self.skip_whitespace();
        let mut values = Vec::new();
        if self.consume_byte(b']') {
            return Ok(JsonNode::Array(values));
        }
        loop {
            values.push(self.parse_value(depth + 1)?);
            self.skip_whitespace();
            if self.consume_byte(b']') {
                return Ok(JsonNode::Array(values));
            }
            if !self.consume_byte(b',') {
                return Err(Self::failure(JsonFailureKind::InvalidSyntax, self.index));
            }
            self.skip_whitespace();
        }
    }

    fn parse_object(&mut self, depth: usize) -> Result<JsonNode, JsonFailure> {
        if depth >= JSON_DEPTH_LIMIT {
            return Err(Self::failure(JsonFailureKind::DepthLimit, self.index));
        }
        self.index += 1;
        self.skip_whitespace();
        let mut values = BTreeMap::new();
        if self.consume_byte(b'}') {
            return Ok(JsonNode::Object(values));
        }
        loop {
            let key_offset = self.index;
            if self.bytes.get(self.index) != Some(&b'"') {
                return Err(Self::failure(JsonFailureKind::InvalidSyntax, self.index));
            }
            let key = self.parse_string()?;
            if values.contains_key(&key) {
                return Err(Self::failure(JsonFailureKind::InvalidSyntax, key_offset));
            }
            self.skip_whitespace();
            if !self.consume_byte(b':') {
                return Err(Self::failure(JsonFailureKind::InvalidSyntax, self.index));
            }
            self.skip_whitespace();
            values.insert(key, self.parse_value(depth + 1)?);
            self.skip_whitespace();
            if self.consume_byte(b'}') {
                return Ok(JsonNode::Object(values));
            }
            if !self.consume_byte(b',') {
                return Err(Self::failure(JsonFailureKind::InvalidSyntax, self.index));
            }
            self.skip_whitespace();
        }
    }

    fn parse_string(&mut self) -> Result<String, JsonFailure> {
        let start = self.index;
        if !self.consume_byte(b'"') {
            return Err(Self::failure(JsonFailureKind::InvalidSyntax, start));
        }
        let mut output = String::new();
        loop {
            let Some(byte) = self.bytes.get(self.index).copied() else {
                return Err(Self::failure(JsonFailureKind::InvalidSyntax, start));
            };
            match byte {
                b'"' => {
                    self.index += 1;
                    return Ok(output);
                }
                b'\\' => {
                    self.index += 1;
                    self.parse_escape(&mut output)?;
                }
                0..=0x1f => {
                    return Err(Self::failure(JsonFailureKind::InvalidSyntax, self.index));
                }
                _ => {
                    let Some(value) = self.source[self.index..].chars().next() else {
                        return Err(Self::failure(JsonFailureKind::InvalidSyntax, self.index));
                    };
                    output.push(value);
                    self.index += value.len_utf8();
                }
            }
        }
    }

    fn parse_escape(&mut self, output: &mut String) -> Result<(), JsonFailure> {
        let offset = self.index;
        let Some(byte) = self.bytes.get(self.index).copied() else {
            return Err(Self::failure(JsonFailureKind::InvalidSyntax, offset));
        };
        self.index += 1;
        match byte {
            b'"' => output.push('"'),
            b'\\' => output.push('\\'),
            b'/' => output.push('/'),
            b'b' => output.push('\u{0008}'),
            b'f' => output.push('\u{000c}'),
            b'n' => output.push('\n'),
            b'r' => output.push('\r'),
            b't' => output.push('\t'),
            b'u' => {
                let first = self.parse_hex_quad()?;
                let scalar = if (0xd800..=0xdbff).contains(&first) {
                    if !self.consume_byte(b'\\') || !self.consume_byte(b'u') {
                        return Err(Self::failure(JsonFailureKind::InvalidSyntax, offset));
                    }
                    let second = self.parse_hex_quad()?;
                    if !(0xdc00..=0xdfff).contains(&second) {
                        return Err(Self::failure(JsonFailureKind::InvalidSyntax, offset));
                    }
                    0x1_0000 + ((u32::from(first) - 0xd800) << 10) + (u32::from(second) - 0xdc00)
                } else if (0xdc00..=0xdfff).contains(&first) {
                    return Err(Self::failure(JsonFailureKind::InvalidSyntax, offset));
                } else {
                    u32::from(first)
                };
                let Some(scalar) = char::from_u32(scalar) else {
                    return Err(Self::failure(JsonFailureKind::InvalidSyntax, offset));
                };
                output.push(scalar);
            }
            _ => return Err(Self::failure(JsonFailureKind::InvalidSyntax, offset)),
        }
        Ok(())
    }

    fn parse_hex_quad(&mut self) -> Result<u16, JsonFailure> {
        let offset = self.index;
        let Some(bytes) = self.bytes.get(self.index..self.index.saturating_add(4)) else {
            return Err(Self::failure(JsonFailureKind::InvalidSyntax, offset));
        };
        let mut value = 0_u16;
        for byte in bytes {
            let digit = match byte {
                b'0'..=b'9' => u16::from(*byte - b'0'),
                b'a'..=b'f' => u16::from(*byte - b'a' + 10),
                b'A'..=b'F' => u16::from(*byte - b'A' + 10),
                _ => return Err(Self::failure(JsonFailureKind::InvalidSyntax, offset)),
            };
            value = (value << 4) | digit;
        }
        self.index += 4;
        Ok(value)
    }

    fn parse_number(&mut self) -> Result<JsonNode, JsonFailure> {
        let start = self.index;
        self.consume_byte(b'-');
        match self.bytes.get(self.index).copied() {
            Some(b'0') => {
                self.index += 1;
                if self.bytes.get(self.index).is_some_and(u8::is_ascii_digit) {
                    return Err(Self::failure(JsonFailureKind::InvalidSyntax, self.index));
                }
            }
            Some(b'1'..=b'9') => {
                self.index += 1;
                while self.bytes.get(self.index).is_some_and(u8::is_ascii_digit) {
                    self.index += 1;
                }
            }
            _ => return Err(Self::failure(JsonFailureKind::InvalidSyntax, self.index)),
        }
        if self.consume_byte(b'.') {
            let fraction = self.index;
            while self.bytes.get(self.index).is_some_and(u8::is_ascii_digit) {
                self.index += 1;
            }
            if self.index == fraction {
                return Err(Self::failure(JsonFailureKind::InvalidSyntax, self.index));
            }
        }
        if matches!(self.bytes.get(self.index), Some(b'e' | b'E')) {
            self.index += 1;
            if matches!(self.bytes.get(self.index), Some(b'+' | b'-')) {
                self.index += 1;
            }
            let exponent = self.index;
            while self.bytes.get(self.index).is_some_and(u8::is_ascii_digit) {
                self.index += 1;
            }
            if self.index == exponent {
                return Err(Self::failure(JsonFailureKind::InvalidSyntax, self.index));
            }
        }
        let Ok(value) = self.source[start..self.index].parse::<f64>() else {
            return Err(Self::failure(JsonFailureKind::NumberOutOfRange, start));
        };
        if value.is_finite() {
            Ok(JsonNode::Number(value))
        } else {
            Err(Self::failure(JsonFailureKind::NumberOutOfRange, start))
        }
    }

    fn consume_keyword(&mut self, keyword: &[u8]) -> Result<(), JsonFailure> {
        let start = self.index;
        if self.bytes.get(start..start.saturating_add(keyword.len())) == Some(keyword) {
            self.index += keyword.len();
            Ok(())
        } else {
            Err(Self::failure(JsonFailureKind::InvalidSyntax, start))
        }
    }

    fn skip_whitespace(&mut self) {
        while matches!(
            self.bytes.get(self.index),
            Some(b' ' | b'\n' | b'\r' | b'\t')
        ) {
            self.index += 1;
        }
    }

    fn consume_byte(&mut self, byte: u8) -> bool {
        if self.bytes.get(self.index) == Some(&byte) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn failure(kind: JsonFailureKind, offset: usize) -> JsonFailure {
        JsonFailure { kind, offset }
    }
}

fn format_json_value(
    value: &JsonNode,
    depth: usize,
    output: &mut String,
) -> Result<(), JsonFailure> {
    match value {
        JsonNode::Null => output.push_str("null"),
        JsonNode::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        JsonNode::Number(value) => {
            if !value.is_finite() {
                return Err(JsonFailure {
                    kind: JsonFailureKind::NonFiniteNumber,
                    offset: 0,
                });
            }
            output.push_str(&value.to_string());
        }
        JsonNode::Text(value) => output.push_str(&escape_json_text(value)),
        JsonNode::Array(values) => {
            if depth >= JSON_DEPTH_LIMIT {
                return Err(JsonFailure {
                    kind: JsonFailureKind::DepthLimit,
                    offset: 0,
                });
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
                return Err(JsonFailure {
                    kind: JsonFailureKind::DepthLimit,
                    offset: 0,
                });
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

fn scalar_value(tag: u64, scalar: u64) -> ValueSlot {
    let mut value = ValueSlot::default();
    value.words[0] = tag;
    value.words[3] = scalar;
    value
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

fn json_slot(value: JsonNode, json_type: u64, text_map_type: u64) -> ValueSlot {
    match value {
        JsonNode::Null => enum_value(json_type, 0, Vec::new()),
        JsonNode::Bool(value) => enum_value(
            json_type,
            1,
            vec![scalar_value(VALUE_TAG_BOOL, u64::from(value))],
        ),
        JsonNode::Number(value) => enum_value(
            json_type,
            2,
            vec![scalar_value(VALUE_TAG_FLOAT, value.to_bits())],
        ),
        JsonNode::Text(value) => enum_value(json_type, 3, vec![text_value(&value)]),
        JsonNode::Array(values) => {
            let mut list = ValueSlot::default();
            list.words[0] = VALUE_TAG_LIST;
            let roots = RuntimeRootScope::from_values(vec![list, ValueSlot::default()])
                .unwrap_or_else(|_| std::process::abort());
            let stream = NodeStream::new(&roots, 0, list);
            for value in values.into_iter().rev() {
                roots.write(1, json_slot(value, json_type, text_map_type));
                if stream.prepend(1) != GC_OK {
                    std::process::abort();
                }
            }
            enum_value(json_type, 4, vec![roots.read(0)])
        }
        JsonNode::Object(values) => {
            let mut map = ValueSlot::default();
            map.words[0] = VALUE_TAG_RECORD;
            map.words[1] = text_map_type;
            let roots = RuntimeRootScope::from_values(vec![
                map,
                ValueSlot::default(),
                ValueSlot::default(),
            ])
            .unwrap_or_else(|_| std::process::abort());
            let stream = NodeStream::new(&roots, 0, map);
            for (key, value) in values.into_iter().rev() {
                roots.write(1, text_value(&key));
                roots.write(2, json_slot(value, json_type, text_map_type));
                // Prepend value first and key second to publish key,value order.
                if stream.prepend(2) != GC_OK || stream.prepend(1) != GC_OK {
                    std::process::abort();
                }
            }
            enum_value(json_type, 5, vec![roots.read(0)])
        }
    }
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

fn json_error_slot(error: JsonFailure, json_error_type: u64) -> ValueSlot {
    let (variant, payload) = match error.kind {
        JsonFailureKind::InvalidSyntax => (
            0,
            vec![scalar_value(
                VALUE_TAG_INT,
                i64::try_from(error.offset)
                    .unwrap_or(i64::MAX)
                    .cast_unsigned(),
            )],
        ),
        JsonFailureKind::NumberOutOfRange => (
            1,
            vec![scalar_value(
                VALUE_TAG_INT,
                i64::try_from(error.offset)
                    .unwrap_or(i64::MAX)
                    .cast_unsigned(),
            )],
        ),
        JsonFailureKind::DepthLimit => (2, Vec::new()),
        JsonFailureKind::NonFiniteNumber => (3, Vec::new()),
    };
    enum_value(json_error_type, variant, payload)
}

#[unsafe(export_name = "loom_runtime_json_parse")]
pub unsafe extern "C" fn json_parse(
    data: *const c_void,
    length: u64,
    result_type: u64,
    json_type: u64,
    json_error_type: u64,
    text_map_type: u64,
    output: *mut c_void,
) -> i32 {
    let Some(bytes) = (unsafe { input_bytes(data, length) }) else {
        return STANDARD_INVALID_ARGUMENT;
    };
    let Ok(source) = std::str::from_utf8(bytes) else {
        return STANDARD_INVALID_ARGUMENT;
    };
    let value = match parse_json(source) {
        Ok(value) => result_value(
            result_type,
            true,
            json_slot(value, json_type, text_map_type),
        ),
        Err(error) => result_value(result_type, false, json_error_slot(error, json_error_type)),
    };
    if output.is_null() {
        return STANDARD_INVALID_ARGUMENT;
    }
    unsafe { output.cast::<ValueSlot>().write(value) };
    0
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
            let error = JsonFailure {
                kind: JsonFailureKind::DepthLimit,
                offset: 0,
            };
            let result = result_value(result_type, false, json_error_slot(error, json_error_type));
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
    fn nested_json_survives_every_allocator_collection() {
        let runtime = ActiveRuntime::new();
        let roots = RuntimeRootScope::with_count(2).expect("runtime root scope");
        let document = br#"{"items":["a","b"],"nested":{"key":"value"}}"#;
        unsafe {
            (*runtime.0).heap.collect_before_every_allocation = true;
            assert_eq!(
                json_parse(
                    document.as_ptr().cast(),
                    document.len() as u64,
                    1,
                    16,
                    17,
                    15,
                    roots.pointer(0).cast(),
                ),
                0,
            );
            let parsed_result = roots.read(0);
            let parsed_payload = node_values(
                parsed_result.words[4] as *const ValueNode,
                parsed_result.words[3],
            )
            .unwrap();
            roots.write(0, parsed_payload[0]);
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
            assert_eq!(text::text_value_bytes(&formatted[0]), Some(&document[..]));
            assert!((*runtime.0).heap.collections > 10);
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
    fn json_is_canonical_bounded_and_reports_byte_offsets() {
        let duplicate = parse_json("{\"界\":1,\"界\":2}").unwrap_err();
        assert_eq!(duplicate.kind, JsonFailureKind::InvalidSyntax);
        assert_eq!(duplicate.offset, 9);

        let overflow = parse_json("1e999").unwrap_err();
        assert_eq!(overflow.kind, JsonFailureKind::NumberOutOfRange);
        assert_eq!(overflow.offset, 0);

        let at_limit = format!("{}null{}", "[".repeat(128), "]".repeat(128));
        assert!(parse_json(&at_limit).is_ok());
        let beyond_limit = format!("{}null{}", "[".repeat(129), "]".repeat(129));
        assert_eq!(
            parse_json(&beyond_limit).unwrap_err().kind,
            JsonFailureKind::DepthLimit,
        );

        let mut object = BTreeMap::new();
        object.insert("z".to_owned(), JsonNode::Number(-0.0));
        object.insert("a".to_owned(), JsonNode::Text("line\n".to_owned()));
        assert_eq!(
            format_json(&JsonNode::Object(object)).unwrap(),
            "{\"a\":\"line\\n\",\"z\":-0}",
        );
        assert_eq!(
            format_json(&JsonNode::Number(f64::INFINITY))
                .unwrap_err()
                .kind,
            JsonFailureKind::NonFiniteNumber,
        );

        let mut nested = JsonNode::Null;
        for _ in 0..128 {
            nested = JsonNode::Array(vec![nested]);
        }
        assert!(format_json(&nested).is_ok());
        nested = JsonNode::Array(vec![nested]);
        assert_eq!(
            format_json(&nested).unwrap_err().kind,
            JsonFailureKind::DepthLimit,
        );
    }

    #[test]
    fn native_map_and_json_abi_preserve_shapes_and_depth_errors() {
        let _runtime = ActiveRuntime::new();
        let empty = build_map(15, Vec::new());
        let key = text_value("key");
        let value = scalar_value(VALUE_TAG_INT, 42);
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

            let mut parsed = ValueSlot::default();
            let duplicate = b"{\"a\":1,\"a\":2}";
            assert_eq!(
                json_parse(
                    duplicate.as_ptr().cast(),
                    duplicate.len() as u64,
                    1,
                    16,
                    17,
                    15,
                    (&raw mut parsed).cast(),
                ),
                0,
            );
            assert_eq!(parsed.words[0], VALUE_TAG_ENUM);
            assert_eq!(parsed.words[2], 1);
            let error = node_values(parsed.words[4] as *const ValueNode, 1).unwrap()[0];
            assert_eq!(error.words[1], 17);
            assert_eq!(error.words[2], 0);
            let offset = node_values(error.words[4] as *const ValueNode, 1).unwrap()[0];
            assert_eq!(offset.words[3], 7);

            let mut deep = JsonNode::Null;
            for _ in 0..129 {
                deep = JsonNode::Array(vec![deep]);
            }
            let deep = json_slot(deep, 16, 15);
            let mut formatted = ValueSlot::default();
            assert_eq!(
                json_format(
                    (&raw const deep).cast(),
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
