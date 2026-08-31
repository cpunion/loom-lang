//! Representation-neutral canonical JSON formatting utilities.

use std::collections::BTreeMap;

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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{JsonFormatFailure, JsonNode, format_json};

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
}
