//! Process boundary used by compiler-known `standard.process` operations.

use std::ffi::{CStr, c_char, c_void};
use std::ptr;
use std::slice;
use std::sync::OnceLock;

use crate::gc::allocate_value_node;
use crate::scheduler::{ValueNode, ValueSlot};
use loom_runtime_abi::VALUE_TAG_LIST;

static ARGUMENTS: OnceLock<Vec<String>> = OnceLock::new();

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
        let Some(value) = crate::gc::text_value(argument.as_bytes()) else {
            return 1;
        };
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

/// Looks up a Unicode environment variable and returns one managed Text object.
/// A null return denotes a missing or non-Unicode value.
#[unsafe(export_name = "loom_runtime_process_environment")]
pub unsafe extern "C" fn process_environment(name: *const u8, name_length: u64) -> *mut c_void {
    if name.is_null() {
        return ptr::null_mut();
    }
    let Ok(name_length) = usize::try_from(name_length) else {
        return ptr::null_mut();
    };
    // SAFETY: generated Text supplies a readable byte range of its stored length.
    let Ok(name) = std::str::from_utf8(unsafe { slice::from_raw_parts(name, name_length) }) else {
        return ptr::null_mut();
    };
    let Ok(value) = std::env::var(name) else {
        return ptr::null_mut();
    };
    crate::gc::retain_text(value.as_bytes()).unwrap_or(ptr::null_mut())
}
