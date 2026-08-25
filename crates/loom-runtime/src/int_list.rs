//! Compiler-private contiguous storage for synchronous, concrete `List[Int]`.
//!
//! Generated LLVM owns the header and inlines element append/access. The
//! runtime is only responsible for growing and releasing the allocation. This
//! representation is deliberately separate from the universal `Value`
//! list and from the managed heap: a proven synchronous local list neither
//! needs GC roots nor an executor/reactor.

use std::mem::{self, size_of};
use std::process;
use std::ptr;

use loom_runtime_abi::{WAIT_INVALID_ARGUMENT, WAIT_OK};

const INITIAL_CAPACITY: usize = 8;
const MAX_CAPACITY: usize = (isize::MAX as usize) / size_of::<i64>();

/// Owned contiguous storage used by the compiler's synchronous `List[Int]`
/// fast path.
///
/// The all-zero header is the canonical empty value. A non-empty allocation
/// has `data != null`, `len <= capacity`, and capacity no larger than
/// [`MAX_CAPACITY`]. Only this module may allocate or free `data`.
///
/// This is a compiler-private ABI, not a source or FFI type. In particular,
/// callers must not forge a non-null `data` pointer: pointer provenance cannot
/// be validated from a C ABI header.
#[repr(C)]
#[derive(Debug)]
pub struct LoomIntListStorage {
    pub data: *mut i64,
    pub len: u64,
    pub capacity: u64,
}

impl Default for LoomIntListStorage {
    fn default() -> Self {
        Self {
            data: ptr::null_mut(),
            len: 0,
            capacity: 0,
        }
    }
}

#[derive(Clone, Copy)]
struct ValidatedHeader {
    len: usize,
    capacity: usize,
}

fn checked_capacity(value: u64) -> Option<usize> {
    let value = usize::try_from(value).ok()?;
    (value <= MAX_CAPACITY).then_some(value)
}

fn validate(storage: &LoomIntListStorage) -> Option<ValidatedHeader> {
    let len = usize::try_from(storage.len).ok()?;
    let capacity = checked_capacity(storage.capacity)?;
    if len > capacity || storage.data.is_null() != (capacity == 0) {
        return None;
    }
    Some(ValidatedHeader { len, capacity })
}

fn growth_capacity(current: usize, minimum: u64) -> Option<usize> {
    let minimum = checked_capacity(minimum)?;
    if minimum <= current {
        return Some(current);
    }
    let doubled = current.checked_mul(2).unwrap_or(MAX_CAPACITY);
    Some(minimum.max(INITIAL_CAPACITY).max(doubled).min(MAX_CAPACITY))
}

unsafe fn take(storage: &mut LoomIntListStorage, validated: ValidatedHeader) -> Vec<i64> {
    let data = storage.data;
    *storage = LoomIntListStorage::default();
    if validated.capacity == 0 {
        return Vec::new();
    }
    // SAFETY: validate checked the structural invariants. The private ABI
    // additionally requires that non-null data came from a previous call to
    // int_list_reserve and has not been freed or aliased into another owner.
    unsafe { Vec::from_raw_parts(data, validated.len, validated.capacity) }
}

fn put(storage: &mut LoomIntListStorage, mut values: Vec<i64>) {
    debug_assert!(values.capacity() <= MAX_CAPACITY);
    if values.capacity() == 0 {
        *storage = LoomIntListStorage::default();
        return;
    }
    storage.data = values.as_mut_ptr();
    storage.len = values.len() as u64;
    storage.capacity = values.capacity() as u64;
    mem::forget(values);
}

/// Ensures that `storage` can hold at least `minimum_capacity` `Int` values.
///
/// Returns `WAIT_INVALID_ARGUMENT` only for a null or structurally invalid
/// compiler-private header. Capacity overflow or allocation failure is Loom's
/// unrecoverable process-level OOM fault and aborts immediately; it must never
/// become a language `Result` or ordinary function status.
#[unsafe(export_name = "loom_int_list_reserve_v1")]
pub unsafe extern "C" fn int_list_reserve(
    storage: *mut LoomIntListStorage,
    minimum_capacity: u64,
) -> i32 {
    let Some(storage) = (unsafe { storage.as_mut() }) else {
        return WAIT_INVALID_ARGUMENT;
    };
    let Some(validated) = validate(storage) else {
        return WAIT_INVALID_ARGUMENT;
    };
    let Some(target) = growth_capacity(validated.capacity, minimum_capacity) else {
        process::abort();
    };
    if target == validated.capacity {
        return WAIT_OK;
    }

    // SAFETY: validation above checked the header before taking ownership.
    let mut values = unsafe { take(storage, validated) };
    let additional = target - values.len();
    if values.try_reserve_exact(additional).is_err() {
        process::abort();
    }
    put(storage, values);
    WAIT_OK
}

/// Releases a storage allocation and restores the canonical zero header.
///
/// Dropping the zero header is valid and idempotent. As with reserve, forged
/// non-null data violates the private ABI and is outside the safe validation
/// boundary.
#[unsafe(export_name = "loom_int_list_drop_v1")]
pub unsafe extern "C" fn int_list_drop(storage: *mut LoomIntListStorage) -> i32 {
    let Some(storage) = (unsafe { storage.as_mut() }) else {
        return WAIT_INVALID_ARGUMENT;
    };
    let Some(validated) = validate(storage) else {
        return WAIT_INVALID_ARGUMENT;
    };
    // SAFETY: validation above checked the header before taking ownership.
    drop(unsafe { take(storage, validated) });
    WAIT_OK
}

#[cfg(test)]
mod tests {
    use std::mem::{align_of, size_of};
    use std::os::raw::c_int;
    use std::os::unix::process::CommandExt;
    use std::process::Command;
    use std::ptr::NonNull;

    use loom_runtime_abi::{WAIT_INVALID_ARGUMENT, WAIT_OK};

    use super::{LoomIntListStorage, growth_capacity, int_list_drop, int_list_reserve};

    const ABORT_CHILD_ENV: &str = "LOOM_RUNTIME_INT_LIST_ABORT_TEST_CHILD";

    unsafe extern "C" {
        fn close(descriptor: c_int) -> c_int;
    }

    unsafe fn push(storage: &mut LoomIntListStorage, value: i64) {
        assert_eq!(
            unsafe { int_list_reserve(storage, storage.len + 1) },
            WAIT_OK
        );
        let index = usize::try_from(storage.len).expect("test length must fit usize");
        // SAFETY: reserve established capacity for this new initialized slot.
        unsafe { storage.data.add(index).write(value) };
        storage.len += 1;
    }

    unsafe fn values(storage: &LoomIntListStorage) -> &[i64] {
        if storage.len == 0 {
            return &[];
        }
        // SAFETY: test storage is exclusively runtime-created and all elements
        // below len were initialized by push.
        unsafe {
            std::slice::from_raw_parts(
                storage.data,
                usize::try_from(storage.len).expect("test length must fit usize"),
            )
        }
    }

    #[test]
    fn header_layout_and_zero_drop_are_stable() {
        assert_eq!(size_of::<LoomIntListStorage>(), 24);
        assert_eq!(align_of::<LoomIntListStorage>(), 8);

        let mut storage = LoomIntListStorage::default();
        assert!(storage.data.is_null());
        assert_eq!(unsafe { int_list_drop(&raw mut storage) }, WAIT_OK);
        assert_eq!(unsafe { int_list_drop(&raw mut storage) }, WAIT_OK);
        assert!(storage.data.is_null());
        assert_eq!(storage.len, 0);
        assert_eq!(storage.capacity, 0);
    }

    #[test]
    fn geometric_growth_preserves_element_order() {
        let mut storage = LoomIntListStorage::default();
        let mut observed_capacities = Vec::new();
        for value in 0..257_i64 {
            let old_capacity = storage.capacity;
            unsafe { push(&mut storage, value * 3 - 7) };
            if storage.capacity != old_capacity {
                observed_capacities.push(storage.capacity);
            }
        }

        let expected = (0..257_i64).map(|value| value * 3 - 7).collect::<Vec<_>>();
        assert_eq!(unsafe { values(&storage) }, expected);
        assert!(observed_capacities.len() > 2);
        assert!(
            observed_capacities
                .windows(2)
                .all(|pair| pair[1] >= pair[0] * 2)
        );
        assert_eq!(unsafe { int_list_drop(&raw mut storage) }, WAIT_OK);
        assert!(storage.data.is_null());
    }

    #[test]
    fn repeated_reserve_within_capacity_does_not_reallocate_or_change_length() {
        let mut storage = LoomIntListStorage::default();
        assert_eq!(unsafe { int_list_reserve(&raw mut storage, 33) }, WAIT_OK);
        let data = storage.data;
        let capacity = storage.capacity;
        for minimum in [0, 1, 8, 32, 33, capacity] {
            assert_eq!(
                unsafe { int_list_reserve(&raw mut storage, minimum) },
                WAIT_OK
            );
            assert_eq!(storage.data, data);
            assert_eq!(storage.capacity, capacity);
            assert_eq!(storage.len, 0);
        }
        assert_eq!(unsafe { int_list_drop(&raw mut storage) }, WAIT_OK);
    }

    #[test]
    fn structurally_invalid_headers_are_rejected_before_touching_data() {
        assert_eq!(
            unsafe { int_list_reserve(std::ptr::null_mut(), 1) },
            WAIT_INVALID_ARGUMENT
        );
        assert_eq!(
            unsafe { int_list_drop(std::ptr::null_mut()) },
            WAIT_INVALID_ARGUMENT
        );

        let dangling = NonNull::<i64>::dangling().as_ptr();
        let mut null_with_capacity = LoomIntListStorage {
            data: std::ptr::null_mut(),
            len: 0,
            capacity: 1,
        };
        let mut data_without_capacity = LoomIntListStorage {
            data: dangling,
            len: 0,
            capacity: 0,
        };
        let mut length_over_capacity = LoomIntListStorage {
            data: dangling,
            len: 2,
            capacity: 1,
        };
        for storage in [
            &raw mut null_with_capacity,
            &raw mut data_without_capacity,
            &raw mut length_over_capacity,
        ] {
            assert_eq!(
                unsafe { int_list_reserve(storage, 2) },
                WAIT_INVALID_ARGUMENT
            );
            assert_eq!(unsafe { int_list_drop(storage) }, WAIT_INVALID_ARGUMENT);
        }
    }

    #[test]
    fn impossible_capacity_is_classified_before_allocation() {
        assert_eq!(growth_capacity(0, u64::MAX), None);
    }

    #[test]
    fn impossible_capacity_aborts_the_process() {
        if std::env::var_os(ABORT_CHILD_ENV).is_some() {
            let mut storage = LoomIntListStorage::default();
            let _ = unsafe { int_list_reserve(&raw mut storage, u64::MAX) };
            panic!("impossible reserve unexpectedly returned");
        }

        let mut child = Command::new(std::env::current_exe().expect("test executable must exist"));
        child
            .arg("--exact")
            .arg("int_list::tests::impossible_capacity_aborts_the_process")
            .arg("--nocapture")
            .env(ABORT_CHILD_ENV, "1");
        // Other runtime tests intentionally hold raw resource descriptors.
        // Close inherited descriptors between fork and exec so this abort
        // probe cannot keep an unrelated socket alive when tests run in
        // parallel. `close` is async-signal-safe and invalid descriptors are
        // harmless here.
        unsafe {
            child.pre_exec(|| {
                for descriptor in 3..4096 {
                    // SAFETY: closing an inherited descriptor is the purpose
                    // of this isolated child hook; the child does not access
                    // parent-owned runtime resources.
                    let _ = close(descriptor);
                }
                Ok(())
            });
        }
        let status = child.status().expect("abort test child must launch");
        assert!(!status.success());
    }
}
