//! Structured logging from direct typed `Text` and `TextMap` storage.

use std::ffi::c_void;
use std::mem::{align_of, size_of};
use std::slice;

use loom_runtime_abi::{
    GC_MAX_REPEATED_POINTER_CELLS, LoomTypedLogField, TYPED_LOG_INVALID_ARGUMENT, TYPED_LOG_OK,
};

use crate::output::write_process_stderr;
use crate::text::text_bytes;

const LOG_LEVELS: [&str; 4] = ["debug", "info", "warn", "error"];

fn escape_log_text(value: &str) -> String {
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

unsafe fn field_slice<'fields>(
    fields: *const LoomTypedLogField,
    field_count: u64,
) -> Option<&'fields [LoomTypedLogField]> {
    if field_count == 0 {
        return fields.is_null().then_some(&[]);
    }
    if fields.is_null()
        || !fields
            .addr()
            .is_multiple_of(align_of::<LoomTypedLogField>())
        || field_count > GC_MAX_REPEATED_POINTER_CELLS / 2
    {
        return None;
    }
    let field_count = usize::try_from(field_count).ok()?;
    let byte_length = field_count.checked_mul(size_of::<LoomTypedLogField>())?;
    if byte_length > isize::MAX as usize {
        return None;
    }
    // SAFETY: this private synchronous ABI requires the aligned entry range to
    // remain readable for the complete call. The range extent was checked
    // above, and generated code passes immutable TextMap[Text] entry storage.
    Some(unsafe { slice::from_raw_parts(fields, field_count) })
}

unsafe fn format_typed_log_line(
    level: u32,
    message: *const c_void,
    fields: *const LoomTypedLogField,
    field_count: u64,
) -> Option<String> {
    let level = LOG_LEVELS.get(usize::try_from(level).ok()?)?;
    // SAFETY: the private ABI requires a complete direct Text object which
    // remains live for this non-collecting call. The runtime validates the
    // Text descriptor and exact object header before borrowing its payload.
    let message = unsafe { text_bytes(message) }?;
    let message = std::str::from_utf8(message).ok()?;
    // SAFETY: the caller supplies the canonical immutable map entry view for
    // the complete synchronous call.
    let fields = unsafe { field_slice(fields, field_count) }?;

    let mut line = String::from("{\"level\":");
    line.push_str(&escape_log_text(level));
    line.push_str(",\"message\":");
    line.push_str(&escape_log_text(message));
    line.push_str(",\"fields\":{");
    let mut previous_key = None;
    for (index, field) in fields.iter().enumerate() {
        // SAFETY: each field is required to contain two complete direct Text
        // object bases which remain live with their owning immutable map.
        let key = unsafe { text_bytes(field.key) }?;
        let value = unsafe { text_bytes(field.value) }?;
        if previous_key.is_some_and(|previous: &[u8]| previous >= key) {
            return None;
        }
        let key_text = std::str::from_utf8(key).ok()?;
        let value_text = std::str::from_utf8(value).ok()?;
        if index != 0 {
            line.push(',');
        }
        line.push_str(&escape_log_text(key_text));
        line.push(':');
        line.push_str(&escape_log_text(value_text));
        previous_key = Some(key);
    }
    line.push_str("}}\n");
    Some(line)
}

/// Writes one compact canonical JSON line to process standard error.
///
/// This boundary performs no Loom allocation, collection, scheduling, or
/// pointer retention. Rust-owned formatting storage is completed before the
/// stderr lock is acquired. A write failure may have emitted a prefix and must
/// not be retried by generated code.
#[unsafe(export_name = "loom_runtime_log_typed_v1")]
pub unsafe extern "C" fn log_typed_v1(
    level: u32,
    message: *const c_void,
    fields: *const LoomTypedLogField,
    field_count: u64,
) -> i32 {
    // SAFETY: all borrowed ABI inputs are consumed before this function
    // returns, and formatting never enters the Loom moving collector.
    let Some(line) = (unsafe { format_typed_log_line(level, message, fields, field_count) }) else {
        return TYPED_LOG_INVALID_ARGUMENT;
    };
    let status = write_process_stderr(line.as_bytes());
    debug_assert!(status != TYPED_LOG_OK || line.ends_with('\n'));
    status
}

#[cfg(test)]
mod tests {
    use std::process::{self, Command};
    use std::ptr;

    use loom_runtime_abi::{TYPED_LOG_INVALID_ARGUMENT, TYPED_LOG_OK};

    use super::*;
    use crate::text::{allocate_byte_storage, allocate_text_storage};

    const LOG_CHILD_ENV: &str = "LOOM_RUNTIME_TYPED_LOG_TEST_CHILD";

    #[test]
    fn formatter_uses_canonical_order_and_json_escaping_without_value_slots() {
        let (_message_storage, message) =
            allocate_text_storage(b"event\nline\0\"").expect("message Text");
        let (_a_storage, a) = allocate_text_storage(b"a").expect("a Text");
        let (_first_storage, first) = allocate_text_storage(b"first").expect("first Text");
        let (_z_storage, z) = allocate_text_storage(b"z").expect("z Text");
        let (_last_storage, last) = allocate_text_storage("last 界".as_bytes()).expect("last Text");
        let fields = [
            LoomTypedLogField {
                key: a.cast(),
                value: first.cast(),
            },
            LoomTypedLogField {
                key: z.cast(),
                value: last.cast(),
            },
        ];
        // SAFETY: every object and the field array remain live for the call.
        let line = unsafe {
            format_typed_log_line(2, message.cast(), fields.as_ptr(), fields.len() as u64)
        }
        .expect("valid typed log line");
        assert_eq!(
            line,
            "{\"level\":\"warn\",\"message\":\"event\\nline\\u0000\\\"\",\"fields\":{\"a\":\"first\",\"z\":\"last 界\"}}\n"
        );

        let reversed = [fields[1], fields[0]];
        // SAFETY: the range is readable; reversed canonical order is rejected.
        assert!(
            unsafe {
                format_typed_log_line(2, message.cast(), reversed.as_ptr(), reversed.len() as u64)
            }
            .is_none()
        );
    }

    #[test]
    fn typed_boundary_rejects_noncanonical_shapes_before_writing() {
        let (_message_storage, message) = allocate_text_storage(b"message").expect("message Text");
        let (_key_storage, key) = allocate_text_storage(b"key").expect("key Text");
        let (_value_storage, value) = allocate_text_storage(b"value").expect("value Text");
        let field = LoomTypedLogField {
            key: key.cast(),
            value: value.cast(),
        };
        let (_bytes_storage, bytes) = allocate_byte_storage(b"bytes").expect("Bytes object");
        // SAFETY: every case is rejected from scalar, null, alignment, extent,
        // descriptor, or field validation before an invalid range is read.
        unsafe {
            assert_eq!(
                log_typed_v1(4, message.cast(), ptr::null(), 0),
                TYPED_LOG_INVALID_ARGUMENT
            );
            assert_eq!(
                log_typed_v1(0, ptr::null(), ptr::null(), 0),
                TYPED_LOG_INVALID_ARGUMENT
            );
            assert_eq!(
                log_typed_v1(0, bytes.cast(), ptr::null(), 0),
                TYPED_LOG_INVALID_ARGUMENT
            );
            assert_eq!(
                log_typed_v1(0, message.cast(), &raw const field, 0),
                TYPED_LOG_INVALID_ARGUMENT
            );
            assert_eq!(
                log_typed_v1(0, message.cast(), ptr::null(), 1),
                TYPED_LOG_INVALID_ARGUMENT
            );
            assert_eq!(
                log_typed_v1(0, message.cast(), message.cast::<u8>().add(1).cast(), 1,),
                TYPED_LOG_INVALID_ARGUMENT
            );
            assert_eq!(
                log_typed_v1(0, message.cast(), &raw const field, u64::MAX),
                TYPED_LOG_INVALID_ARGUMENT
            );
            let missing_key = LoomTypedLogField {
                key: ptr::null(),
                value: value.cast(),
            };
            assert_eq!(
                log_typed_v1(0, message.cast(), &raw const missing_key, 1),
                TYPED_LOG_INVALID_ARGUMENT
            );
        }
    }

    #[test]
    fn concurrent_typed_calls_emit_complete_lf_terminated_stderr_lines() {
        if std::env::var_os(LOG_CHILD_ENV).is_some() {
            let mut storage = Vec::new();
            let mut messages = Vec::new();
            for index in 0..8 {
                let (allocation, message) =
                    allocate_text_storage(format!("message-{index}").as_bytes())
                        .expect("child message Text");
                storage.push(allocation);
                messages.push(message.addr());
            }
            std::thread::scope(|scope| {
                for (index, message) in messages.into_iter().enumerate() {
                    scope.spawn(move || {
                        // SAFETY: the owning allocations outlive this scope;
                        // empty fields use the canonical null/zero pair.
                        let status = unsafe {
                            log_typed_v1(
                                u32::try_from(index % 4).expect("level"),
                                message as *const c_void,
                                ptr::null(),
                                0,
                            )
                        };
                        assert_eq!(status, TYPED_LOG_OK);
                    });
                }
            });
            drop(storage);
            process::exit(0);
        }

        let output = Command::new(std::env::current_exe().expect("runtime test executable"))
            .args([
                "--exact",
                "logging::tests::concurrent_typed_calls_emit_complete_lf_terminated_stderr_lines",
                "--nocapture",
            ])
            .env(LOG_CHILD_ENV, "1")
            .output()
            .expect("run typed-log child");
        assert!(output.status.success(), "{output:?}");
        assert!(!output.stderr.contains(&b'\r'), "{:?}", output.stderr);
        let stderr = String::from_utf8(output.stderr).expect("typed log stderr is UTF-8");
        let mut actual = stderr.lines().map(str::to_owned).collect::<Vec<_>>();
        actual.sort();
        let levels = ["debug", "info", "warn", "error"];
        let mut expected = (0..8)
            .map(|index| {
                format!(
                    "{{\"level\":\"{}\",\"message\":\"message-{index}\",\"fields\":{{}}}}",
                    levels[index % levels.len()]
                )
            })
            .collect::<Vec<_>>();
        expected.sort();
        assert_eq!(actual, expected);
        assert!(stderr.ends_with('\n'));
    }
}
