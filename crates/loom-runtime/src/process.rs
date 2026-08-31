//! Typed process boundary used by compiler-known `std.process` primitives.

#[cfg(not(windows))]
use std::ffi::CStr;
use std::ffi::{c_char, c_void};
#[cfg(not(windows))]
use std::slice;
use std::sync::OnceLock;

use loom_runtime_abi::{
    PROCESS_ARGUMENT_COUNT_TYPED_INVALID, PROCESS_ARGUMENT_TYPED_INVALID,
    PROCESS_ARGUMENT_TYPED_OK, PROCESS_ENVIRONMENT_TYPED_FOUND, PROCESS_ENVIRONMENT_TYPED_INVALID,
    PROCESS_ENVIRONMENT_TYPED_MISSING,
};

static ARGUMENTS: OnceLock<Box<[String]>> = OnceLock::new();

/// Copies the platform arguments into an immutable process snapshot.
///
/// Windows ignores the narrow C entry arguments and reads the operating
/// system's wide argument source through `std::env::args_os`. Valid Unicode is
/// preserved exactly; isolated UTF-16 surrogates become the Unicode replacement
/// character because Loom `Text` contains only Unicode scalar values.
#[unsafe(export_name = "loom_runtime_process_arguments_initialize_typed_v1")]
pub unsafe extern "C" fn arguments_initialize_typed_v1(
    argument_count: i32,
    argument_vector: *const *const c_char,
) -> i32 {
    #[cfg(windows)]
    let arguments = {
        let _ = (argument_count, argument_vector);
        std::env::args_os()
            .skip(1)
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
    };

    #[cfg(not(windows))]
    let arguments = {
        let Ok(count) = usize::try_from(argument_count) else {
            return PROCESS_ARGUMENT_TYPED_INVALID;
        };
        if count != 0 && argument_vector.is_null() {
            return PROCESS_ARGUMENT_TYPED_INVALID;
        }
        let platform_arguments = if count == 0 {
            &[][..]
        } else {
            // SAFETY: the platform C entry contract supplies `argc` readable
            // pointers. Each non-null pointer names one NUL-terminated argument.
            unsafe { slice::from_raw_parts(argument_vector, count) }
        };
        let mut arguments = Vec::with_capacity(count.saturating_sub(1));
        for argument in platform_arguments.iter().skip(1) {
            if argument.is_null() {
                return PROCESS_ARGUMENT_TYPED_INVALID;
            }
            // SAFETY: validated non-null above; termination comes from the C
            // entry contract. Lossy conversion gives Loom valid Unicode Text.
            arguments.push(
                unsafe { CStr::from_ptr(*argument) }
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        arguments
    };

    match ARGUMENTS.set(arguments.into_boxed_slice()) {
        Ok(()) => PROCESS_ARGUMENT_TYPED_OK,
        Err(_) => PROCESS_ARGUMENT_TYPED_INVALID,
    }
}

#[unsafe(export_name = "loom_runtime_process_argument_count_typed_v1")]
pub extern "C" fn argument_count_typed_v1() -> i64 {
    ARGUMENTS
        .get()
        .and_then(|arguments| i64::try_from(arguments.len()).ok())
        .unwrap_or(PROCESS_ARGUMENT_COUNT_TYPED_INVALID)
}

#[unsafe(export_name = "loom_runtime_process_argument_at_typed_v1")]
pub unsafe extern "C" fn argument_at_typed_v1(index: i64, output: *mut *mut c_void) -> i32 {
    if output.is_null() || !output.is_aligned() {
        return PROCESS_ARGUMENT_TYPED_INVALID;
    }
    // Never expose stale caller storage on failure.
    unsafe { output.write(std::ptr::null_mut()) };
    let Some(arguments) = ARGUMENTS.get() else {
        return PROCESS_ARGUMENT_TYPED_INVALID;
    };
    unsafe { argument_at_from(arguments, index, output) }
}

unsafe fn argument_at_from(arguments: &[String], index: i64, output: *mut *mut c_void) -> i32 {
    let Ok(index) = usize::try_from(index) else {
        return PROCESS_ARGUMENT_TYPED_INVALID;
    };
    let Some(argument) = arguments.get(index) else {
        return PROCESS_ARGUMENT_TYPED_INVALID;
    };
    let Ok(scalar_length) = u64::try_from(argument.chars().count()) else {
        return PROCESS_ARGUMENT_TYPED_INVALID;
    };
    let status =
        unsafe { crate::text::allocate_typed_text(argument.as_bytes(), scalar_length, output) };
    if status == loom_runtime_abi::GC_OK {
        PROCESS_ARGUMENT_TYPED_OK
    } else {
        PROCESS_ARGUMENT_TYPED_INVALID
    }
}

#[unsafe(export_name = "loom_runtime_process_environment_typed_v1")]
pub unsafe extern "C" fn environment_typed_v1(
    name: *const c_void,
    output: *mut *mut c_void,
) -> i32 {
    if output.is_null() || !output.is_aligned() {
        return PROCESS_ENVIRONMENT_TYPED_INVALID;
    }
    // Publish only a complete found value; missing and invalid calls leave a
    // canonical null cell for the generated Option constructor.
    unsafe { output.write(std::ptr::null_mut()) };
    let Some(bytes) = (unsafe { crate::text::text_bytes(name) }) else {
        return PROCESS_ENVIRONMENT_TYPED_INVALID;
    };
    let Ok(name) = std::str::from_utf8(bytes).map(str::to_owned) else {
        return PROCESS_ENVIRONMENT_TYPED_INVALID;
    };
    // Copy the lookup result before the allocator can move the input Text.
    let Ok(value) = std::env::var(&name) else {
        return PROCESS_ENVIRONMENT_TYPED_MISSING;
    };
    let Ok(scalar_length) = u64::try_from(value.chars().count()) else {
        return PROCESS_ENVIRONMENT_TYPED_INVALID;
    };
    if unsafe { crate::text::allocate_typed_text(value.as_bytes(), scalar_length, output) }
        != loom_runtime_abi::GC_OK
    {
        return PROCESS_ENVIRONMENT_TYPED_INVALID;
    }
    PROCESS_ENVIRONMENT_TYPED_FOUND
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gc::{
        activate_runtime_v1, deactivate_runtime_v1, typed_root_pop_v1, typed_root_push_v1,
    };
    use crate::runtime::{runtime_create_v1, runtime_destroy_v1};
    use loom_runtime_abi::{
        GC_OK, LoomGcTypedRootDescriptor, LoomGcTypedRootFrame, TYPED_SHADOW_STACK_ABI_VERSION,
    };
    use std::ptr;

    #[test]
    fn argument_allocation_survives_forced_collection() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        unsafe {
            assert_eq!(activate_runtime_v1(runtime), GC_OK);
            let bitmaps = [0_u64, 1_u64];
            let descriptor = LoomGcTypedRootDescriptor {
                abi_version: TYPED_SHADOW_STACK_ABI_VERSION,
                flags: 0,
                slot_count: 1,
                state_count: 2,
                live_bitmap_words: 1,
                live_bitmaps: bitmaps.as_ptr(),
            };
            let mut output: *mut c_void = ptr::null_mut();
            let slots = [(&raw mut output).cast::<c_void>()];
            let mut frame = LoomGcTypedRootFrame {
                abi_version: TYPED_SHADOW_STACK_ABI_VERSION,
                flags: 0,
                state: 1,
                descriptor: &raw const descriptor,
                slots: slots.as_ptr(),
                previous: ptr::null_mut(),
            };
            assert_eq!(typed_root_push_v1(&raw mut frame), GC_OK);
            (*runtime).heap.collect_before_every_allocation = true;
            let arguments = ["first".to_owned(), "界🙂".to_owned()];
            assert_eq!(
                argument_at_from(&arguments, 1, &raw mut output),
                PROCESS_ARGUMENT_TYPED_OK
            );
            assert_eq!(crate::text::text_bytes(output), Some("界🙂".as_bytes()));
            let original = output;
            let mut trigger = ptr::null_mut();
            assert_eq!(
                crate::text::allocate_typed_text(b"trigger", 7, &raw mut trigger),
                GC_OK
            );
            assert_ne!(output, original);
            assert_eq!(crate::text::text_bytes(output), Some("界🙂".as_bytes()));
            assert_eq!(typed_root_pop_v1(&raw mut frame), GC_OK);
            (*runtime).heap.collect_before_every_allocation = false;
            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
    }

    #[test]
    fn environment_statuses_are_transactional_across_forced_collection() {
        let (present_name, present_value) = std::env::vars()
            .next()
            .expect("test process has a Unicode environment variable");
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        unsafe {
            assert_eq!(activate_runtime_v1(runtime), GC_OK);
            // Only the output is live at the environment allocation safepoint.
            // The runtime must consume/stage the input name before collection.
            let bitmaps = [0_u64, 2_u64];
            let descriptor = LoomGcTypedRootDescriptor {
                abi_version: TYPED_SHADOW_STACK_ABI_VERSION,
                flags: 0,
                slot_count: 2,
                state_count: 2,
                live_bitmap_words: 1,
                live_bitmaps: bitmaps.as_ptr(),
            };
            let mut name: *mut c_void = ptr::null_mut();
            let mut output: *mut c_void = ptr::null_mut();
            let slots = [
                (&raw mut name).cast::<c_void>(),
                (&raw mut output).cast::<c_void>(),
            ];
            let mut frame = LoomGcTypedRootFrame {
                abi_version: TYPED_SHADOW_STACK_ABI_VERSION,
                flags: 0,
                state: 1,
                descriptor: &raw const descriptor,
                slots: slots.as_ptr(),
                previous: ptr::null_mut(),
            };
            assert_eq!(typed_root_push_v1(&raw mut frame), GC_OK);
            assert_eq!(
                crate::text::allocate_typed_text(
                    present_name.as_bytes(),
                    present_name.chars().count() as u64,
                    &raw mut name,
                ),
                GC_OK
            );
            (*runtime).heap.collect_before_every_allocation = true;
            assert_eq!(
                environment_typed_v1(name, &raw mut output),
                PROCESS_ENVIRONMENT_TYPED_FOUND
            );
            assert_eq!(
                crate::text::text_bytes(output),
                Some(present_value.as_bytes())
            );
            let original_output = output;
            assert_eq!(
                crate::text::allocate_typed_text(b"trigger", 7, &raw mut name),
                GC_OK
            );
            assert_ne!(output, original_output);
            assert_eq!(
                crate::text::text_bytes(output),
                Some(present_value.as_bytes())
            );

            let missing = format!("LOOM_PROCESS_TYPED_MISSING_{}", std::process::id());
            assert_eq!(
                crate::text::allocate_typed_text(
                    missing.as_bytes(),
                    missing.chars().count() as u64,
                    &raw mut name,
                ),
                GC_OK
            );
            output = ptr::dangling_mut();
            assert_eq!(
                environment_typed_v1(name, &raw mut output),
                PROCESS_ENVIRONMENT_TYPED_MISSING
            );
            assert!(output.is_null());
            output = ptr::dangling_mut();
            assert_eq!(
                environment_typed_v1(ptr::null(), &raw mut output),
                PROCESS_ENVIRONMENT_TYPED_INVALID
            );
            assert!(output.is_null());

            assert_eq!(typed_root_pop_v1(&raw mut frame), GC_OK);
            (*runtime).heap.collect_before_every_allocation = false;
            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
    }
}
