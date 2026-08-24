//! Process boundary used by compiler-known `standard.process` operations.

use std::ffi::{CStr, c_char};
use std::ptr;
use std::slice;
use std::sync::{Mutex, OnceLock};

use crate::gc::allocate_value_node;
use crate::scheduler::{ValueNode, ValueSlot};

const VALUE_TAG_TEXT: u64 = 4;
const VALUE_TAG_LIST: u64 = 12;

static ARGUMENTS: OnceLock<Vec<String>> = OnceLock::new();
static ENVIRONMENT_VALUES: OnceLock<Mutex<Vec<Box<str>>>> = OnceLock::new();

#[unsafe(export_name = "loom_runtime_set_arguments")]
pub unsafe extern "C" fn set_arguments(argument_count: i32, argument_vector: *const *const c_char) {
    let count = usize::try_from(argument_count).unwrap_or(0);
    let arguments = if argument_vector.is_null() || count <= 1 {
        Vec::new()
    } else {
        // SAFETY: the platform C entry contract supplies argc readable argv
        // pointers, each terminated by a null byte.
        unsafe { slice::from_raw_parts(argument_vector, count) }
            .iter()
            .skip(1)
            .filter(|argument| !argument.is_null())
            .map(|argument| {
                // SAFETY: checked above and guaranteed by the C entry ABI.
                unsafe { CStr::from_ptr(*argument) }
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    };
    let _ = ARGUMENTS.set(arguments);
}

#[unsafe(export_name = "loom_runtime_process_arguments")]
pub unsafe extern "C" fn process_arguments(output: *mut ValueSlot) -> i32 {
    if output.is_null() {
        return 1;
    }
    let arguments = ARGUMENTS.get().map_or(&[][..], Vec::as_slice);
    let mut head = ptr::null_mut();
    for argument in arguments.iter().rev() {
        let node = allocate_value_node().cast::<ValueNode>();
        if node.is_null() {
            return 1;
        }
        let mut value = ValueSlot::default();
        value.words[0] = VALUE_TAG_TEXT;
        value.words[2] = argument.len() as u64;
        value.words[4] = argument.as_ptr() as u64;
        // SAFETY: allocate_value_node returned a fresh initialized node.
        unsafe {
            (*node).value = value;
            (*node).next = head;
        }
        head = node;
    }
    let mut list = ValueSlot::default();
    list.words[0] = VALUE_TAG_LIST;
    list.words[2] = arguments.len() as u64;
    list.words[4] = head as u64;
    // SAFETY: output was checked non-null and points to generated storage.
    unsafe { *output = list };
    0
}

/// Looks up a Unicode environment variable and returns process-lifetime text.
/// A null return denotes a missing or non-Unicode value.
#[unsafe(export_name = "loom_runtime_process_environment")]
pub unsafe extern "C" fn process_environment(
    name: *const u8,
    name_length: u64,
    value_length: *mut u64,
) -> *const u8 {
    if name.is_null() || value_length.is_null() {
        return ptr::null();
    }
    let Ok(name_length) = usize::try_from(name_length) else {
        return ptr::null();
    };
    // SAFETY: generated Text supplies a readable byte range of its stored length.
    let Ok(name) = std::str::from_utf8(unsafe { slice::from_raw_parts(name, name_length) }) else {
        return ptr::null();
    };
    let Ok(value) = std::env::var(name) else {
        return ptr::null();
    };
    let values = ENVIRONMENT_VALUES.get_or_init(|| Mutex::new(Vec::new()));
    let Ok(mut values) = values.lock() else {
        return ptr::null();
    };
    let value = value.into_boxed_str();
    let pointer = value.as_ptr();
    // SAFETY: value_length was checked non-null.
    unsafe { *value_length = value.len() as u64 };
    values.push(value);
    pointer
}
