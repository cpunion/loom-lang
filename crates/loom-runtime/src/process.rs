//! Process boundary used by compiler-known `standard.process` operations.

use std::ffi::{CStr, c_char};
use std::slice;
use std::sync::OnceLock;

use crate::gc::{NodeStream, RuntimeRootScope};
use crate::scheduler::ValueSlot;
use loom_runtime_abi::{GC_OK, VALUE_TAG_LIST};

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
    build_arguments(arguments, output)
}

fn build_arguments(arguments: &[String], output: *mut ValueSlot) -> i32 {
    let mut list = ValueSlot::default();
    list.words[0] = VALUE_TAG_LIST;
    let Ok(roots) = RuntimeRootScope::from_values(vec![list, ValueSlot::default()]) else {
        return 1;
    };
    let stream = NodeStream::new(&roots, 0, list);
    for argument in arguments.iter().rev() {
        let Some(value) = crate::gc::text_value(argument.as_bytes()) else {
            return 1;
        };
        roots.write(1, value);
        if stream.prepend(1) != GC_OK {
            return 1;
        }
    }
    // SAFETY: output was checked non-null and points to generated storage.
    unsafe { output.write(roots.read(0)) };
    0
}

/// Looks up a Unicode environment variable in stable caller storage.
/// Returns `1` with a managed Text value, `0` when missing or non-Unicode, and
/// `-1` for invalid ABI input or an inactive runtime.
#[unsafe(export_name = "loom_runtime_process_environment")]
pub unsafe extern "C" fn process_environment(
    name: *const u8,
    name_length: u64,
    output: *mut ValueSlot,
) -> i32 {
    if name.is_null() || output.is_null() {
        return -1;
    }
    let Ok(name_length) = usize::try_from(name_length) else {
        return -1;
    };
    // SAFETY: generated Text supplies a readable byte range of its stored length.
    let Ok(name) = std::str::from_utf8(unsafe { slice::from_raw_parts(name, name_length) }) else {
        return -1;
    };
    let Ok(value) = std::env::var(name) else {
        return 0;
    };
    let Some(value) = crate::gc::text_value(value.as_bytes()) else {
        return -1;
    };
    unsafe { output.write(value) };
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gc::{activate_runtime_v1, deactivate_runtime_v1};
    use crate::runtime::{runtime_create_v1, runtime_destroy_v1};
    use crate::scheduler::ValueNode;
    use loom_runtime_abi::{GC_OK, VALUE_WORD_AUX, VALUE_WORD_DATA};

    #[test]
    fn argument_builder_survives_collection_before_every_allocation() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        unsafe {
            assert_eq!(activate_runtime_v1(runtime), GC_OK);
            let roots = RuntimeRootScope::with_count(1).expect("runtime root scope");
            (*runtime).heap.collect_before_every_allocation = true;
            let arguments = vec!["first".to_owned(), "界🙂".to_owned(), "last".to_owned()];
            assert_eq!(build_arguments(&arguments, roots.pointer(0)), 0);
            assert_eq!(roots.read(0).words[VALUE_WORD_AUX], 3);
            let mut node = roots.read(0).words[VALUE_WORD_DATA] as *const ValueNode;
            for expected in [&b"first"[..], "界🙂".as_bytes(), &b"last"[..]] {
                assert!(!node.is_null());
                assert_eq!(
                    crate::text::text_value_bytes(&(*node).value),
                    Some(expected),
                );
                node = (*node).next;
            }
            assert!(node.is_null());
            (*runtime).heap.collect_before_every_allocation = false;
            drop(roots);
            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
    }

    #[test]
    fn environment_writes_a_stable_text_value_output() {
        let (name, expected) = std::env::vars()
            .next()
            .expect("test process has at least one Unicode environment variable");
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        unsafe {
            assert_eq!(activate_runtime_v1(runtime), GC_OK);
            let roots = RuntimeRootScope::with_count(1).expect("runtime root scope");
            (*runtime).heap.collect_before_every_allocation = true;
            assert_eq!(
                process_environment(name.as_ptr(), name.len() as u64, roots.pointer(0)),
                1,
            );
            let first_address = roots.read(0).words[VALUE_WORD_DATA];
            let _trigger = crate::gc::text_value(b"trigger").expect("managed Text");
            assert_ne!(roots.read(0).words[VALUE_WORD_DATA], first_address);
            assert_eq!(
                crate::text::text_value_bytes(&roots.read(0)),
                Some(expected.as_bytes()),
            );
            (*runtime).heap.collect_before_every_allocation = false;
            drop(roots);
            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
    }
}
