//! Managed immutable `Text` storage shared by universal `Value` envelopes and
//! typed direct `Text` pointers.

use std::ffi::c_void;
use std::mem::{align_of, size_of};
use std::ptr;
use std::slice;

use loom_runtime_abi::{
    BYTES_DECODE_UTF8_TYPED_INVALID_UTF8, GC_INVALID_ARGUMENT, GC_MAX_OBJECT_BYTES, GC_OK,
    GC_RESOURCE_LIMIT, LAYOUT_ABI_VERSION, LAYOUT_FLAG_LEAF, LAYOUT_FLAG_MANAGED_POINTER,
    LAYOUT_FLAG_TRAILING_BYTES, LAYOUT_KIND_BYTES, LAYOUT_KIND_TEXT, LoomGcObjectDescriptor,
    LoomGcTypedRootDescriptor, LoomGcTypedRootFrame, LoomLayoutDescriptor, TEXT_GET_TYPED_FOUND,
    TEXT_GET_TYPED_INVALID, TEXT_GET_TYPED_MISSING, TEXT_OBJECT_HEADER_SIZE, TYPED_GC_ABI_VERSION,
    TYPED_SHADOW_STACK_ABI_VERSION, VALUE_SLOT_WORDS, VALUE_TAG_TEXT, VALUE_WORD_AUX,
    VALUE_WORD_DATA, VALUE_WORD_NOMINAL, VALUE_WORD_SCALAR, VALUE_WORD_TAG, VALUE_WORD_WITNESS,
};

use crate::gc::{allocate_typed_object, typed_root_pop_v1, typed_root_push_v1};
use crate::scheduler::ValueSlot;

/// The one process-wide descriptor referenced by dynamic and literal Text
/// objects. Its address is compiler/runtime-private and is not language RTTI.
#[unsafe(export_name = "loom_layout_text_v1")]
pub static TEXT_LAYOUT_DESCRIPTOR: LoomLayoutDescriptor = LoomLayoutDescriptor {
    abi_version: LAYOUT_ABI_VERSION,
    kind: LAYOUT_KIND_TEXT,
    value_size: size_of::<*const c_void>() as u64,
    value_align: align_of::<*const c_void>() as u64,
    object_header_size: TEXT_OBJECT_HEADER_SIZE,
    object_align: align_of::<TextObject>() as u64,
    flags: LAYOUT_FLAG_MANAGED_POINTER | LAYOUT_FLAG_LEAF | LAYOUT_FLAG_TRAILING_BYTES,
    reserved: 0,
};

/// Arbitrary `Bytes` storage has the same compact allocation shape but a
/// distinct descriptor, so invalid UTF-8 can never masquerade as Text.
#[unsafe(export_name = "loom_layout_bytes_v1")]
pub static BYTES_LAYOUT_DESCRIPTOR: LoomLayoutDescriptor = LoomLayoutDescriptor {
    abi_version: LAYOUT_ABI_VERSION,
    kind: LAYOUT_KIND_BYTES,
    value_size: size_of::<*const c_void>() as u64,
    value_align: align_of::<*const c_void>() as u64,
    object_header_size: TEXT_OBJECT_HEADER_SIZE,
    object_align: align_of::<ByteObject>() as u64,
    flags: LAYOUT_FLAG_MANAGED_POINTER | LAYOUT_FLAG_LEAF | LAYOUT_FLAG_TRAILING_BYTES,
    reserved: 0,
};

/// Prefix of a managed Text allocation. UTF-8 bytes immediately follow this
/// header, keeping a dynamic Text object and its payload in one moving object.
#[repr(C)]
pub(crate) struct TextObject {
    pub(crate) layout: *const LoomLayoutDescriptor,
    pub(crate) allocation_size: u64,
    pub(crate) byte_length: u64,
    pub(crate) scalar_length: u64,
    pub(crate) bytes: [u8; 0],
}

/// Prefix of the distinct arbitrary byte-sequence allocation.
#[repr(C)]
pub(crate) struct ByteObject {
    pub(crate) layout: *const LoomLayoutDescriptor,
    pub(crate) allocation_size: u64,
    pub(crate) byte_length: u64,
    pub(crate) reserved: u64,
    pub(crate) bytes: [u8; 0],
}

const TEXT_OBJECT_HEADER_BYTES: usize = size_of::<TextObject>();
const TEXT_OBJECT_HEADER_WORDS: usize = TEXT_OBJECT_HEADER_BYTES / size_of::<u64>();
/// Concatenates two complete Text objects into one precisely described typed
/// managed leaf. Both UTF-8 payloads are copied into non-GC staging storage
/// before the allocation which may move either input. The fresh object remains
/// unpublished while its header and trailing bytes are initialized.
#[unsafe(export_name = "loom_runtime_text_concat_typed_v1")]
pub unsafe extern "C" fn concat_typed_v1(
    left: *const c_void,
    right: *const c_void,
    output: *mut *mut c_void,
) -> i32 {
    if output.is_null() || !output.addr().is_multiple_of(align_of::<*mut c_void>()) {
        return GC_INVALID_ARGUMENT;
    }
    // Invalid inputs and collection failures never leave a stale publication.
    unsafe { output.write(ptr::null_mut()) };
    let staged = (|| {
        // SAFETY: both immutable objects are read in full before the typed
        // allocator below can enter a Loom collection.
        let left = unsafe { text_bytes(left) }?;
        let right = unsafe { text_bytes(right) }?;
        let byte_length = left.len().checked_add(right.len())?;
        let mut staged = Vec::with_capacity(byte_length);
        staged.extend_from_slice(left);
        staged.extend_from_slice(right);
        let text = std::str::from_utf8(&staged).ok()?;
        let scalar_length = u64::try_from(text.chars().count()).ok()?;
        Some((staged, scalar_length))
    })();
    let Some((staged, scalar_length)) = staged else {
        return GC_INVALID_ARGUMENT;
    };
    unsafe { allocate_typed_text(&staged, scalar_length, output) }
}

/// Returns one Unicode scalar as a freshly allocated direct Text.
///
/// The scalar bytes are copied to stack storage before allocation, so the
/// source may move at the allocator safepoint. Missing indices do not allocate.
#[unsafe(export_name = "loom_runtime_text_get_typed_v1")]
pub unsafe extern "C" fn get_typed_v1(
    source: *const c_void,
    index: i64,
    output: *mut *mut c_void,
) -> i32 {
    if output.is_null() || !output.addr().is_multiple_of(align_of::<*mut c_void>()) {
        return TEXT_GET_TYPED_INVALID;
    }
    unsafe { output.write(ptr::null_mut()) };
    let Some(index) = usize::try_from(index).ok() else {
        return TEXT_GET_TYPED_MISSING;
    };
    let mut encoded = [0_u8; 4];
    let encoded_length = {
        // SAFETY: the complete source is consumed before allocation can move
        // it, and validation rejects Bytes or malformed Text objects.
        let Some(bytes) = (unsafe { text_bytes(source) }) else {
            return TEXT_GET_TYPED_INVALID;
        };
        let Ok(text) = std::str::from_utf8(bytes) else {
            return TEXT_GET_TYPED_INVALID;
        };
        let Some(scalar) = text.chars().nth(index) else {
            return TEXT_GET_TYPED_MISSING;
        };
        scalar.encode_utf8(&mut encoded).len()
    };
    let status = unsafe { allocate_typed_text(&encoded[..encoded_length], 1, output) };
    if status == GC_OK {
        TEXT_GET_TYPED_FOUND
    } else {
        TEXT_GET_TYPED_INVALID
    }
}

/// Concatenates two immutable byte sequences into one direct managed Bytes
/// leaf. A typed Bytes value may reuse an immutable Text object after
/// `encode_utf8`, so both canonical byte-sequence layouts are accepted.
///
/// Both payloads are copied to non-GC staging storage before allocation. The
/// allocator may therefore move or reclaim either input, including when an
/// output root cell previously held one of those inputs. The output is cleared
/// before input validation and receives the initialized object only after the
/// allocation and payload copy are complete.
#[unsafe(export_name = "loom_runtime_bytes_append_typed_v1")]
pub unsafe extern "C" fn bytes_append_typed_v1(
    left: *const c_void,
    right: *const c_void,
    output: *mut *mut c_void,
) -> i32 {
    if output.is_null() || !output.addr().is_multiple_of(align_of::<*mut c_void>()) {
        return GC_INVALID_ARGUMENT;
    }
    unsafe { output.write(ptr::null_mut()) };
    let staged = (|| {
        // SAFETY: both complete immutable payloads are consumed before the
        // typed allocator below can enter a moving collection.
        let left = unsafe { bytes(left) }?;
        let right = unsafe { bytes(right) }?;
        let byte_length = left.len().checked_add(right.len())?;
        enforce_typed_byte_length(byte_length);
        let mut staged = Vec::with_capacity(byte_length);
        staged.extend_from_slice(left);
        staged.extend_from_slice(right);
        Some(staged)
    })();
    let Some(staged) = staged else {
        return GC_INVALID_ARGUMENT;
    };
    unsafe { allocate_typed_bytes(&staged, output) }
}

/// Decodes one immutable byte sequence into a direct managed Text leaf.
///
/// Invalid UTF-8 is the one ordinary negative result. Positive failures retain
/// the shared GC status domain. A canonical Text-backed byte sequence is
/// validated deeply and returned directly; a distinct Bytes payload and its
/// scalar count are staged before allocation, so collection cannot invalidate
/// the source. Every non-success path leaves `output` null.
#[unsafe(export_name = "loom_runtime_bytes_decode_utf8_typed_v1")]
pub unsafe extern "C" fn bytes_decode_utf8_typed_v1(
    source: *const c_void,
    output: *mut *mut c_void,
) -> i32 {
    if output.is_null() || !output.addr().is_multiple_of(align_of::<*mut c_void>()) {
        return GC_INVALID_ARGUMENT;
    }
    unsafe { output.write(ptr::null_mut()) };
    let Some(source_bytes) = (unsafe { bytes(source) }) else {
        return GC_INVALID_ARGUMENT;
    };
    let Some(header) = (unsafe { source.cast::<TextObject>().as_ref() }) else {
        return GC_INVALID_ARGUMENT;
    };
    if header.layout == &raw const TEXT_LAYOUT_DESCRIPTOR {
        let Ok(text) = std::str::from_utf8(source_bytes) else {
            return GC_INVALID_ARGUMENT;
        };
        if u64::try_from(text.chars().count()).ok() != Some(header.scalar_length) {
            return GC_INVALID_ARGUMENT;
        }
        // A typed Bytes value produced by Text.encode_utf8 may share the exact
        // immutable Text object. No allocation or collection is needed to
        // recover Text; storage identity remains compiler-private.
        unsafe { output.write(source.cast_mut()) };
        return GC_OK;
    }
    enforce_typed_byte_length(source_bytes.len());
    let staged = source_bytes.to_vec();
    let Ok(text) = std::str::from_utf8(&staged) else {
        return BYTES_DECODE_UTF8_TYPED_INVALID_UTF8;
    };
    let Ok(scalar_length) = u64::try_from(text.chars().count()) else {
        std::process::abort();
    };
    unsafe { allocate_typed_text(&staged, scalar_length, output) }
}

fn enforce_typed_byte_length(byte_length: usize) {
    let maximum = GC_MAX_OBJECT_BYTES
        .checked_sub(TEXT_OBJECT_HEADER_SIZE)
        .and_then(|maximum| usize::try_from(maximum).ok())
        .unwrap_or_else(|| std::process::abort());
    if byte_length > maximum {
        std::process::abort();
    }
}

unsafe fn allocate_typed_bytes(staged: &[u8], output: *mut *mut c_void) -> i32 {
    let Ok(byte_length) = u64::try_from(staged.len()) else {
        std::process::abort();
    };
    let Some(allocation_size) = TEXT_OBJECT_HEADER_SIZE.checked_add(byte_length) else {
        std::process::abort();
    };
    let descriptor = LoomGcObjectDescriptor {
        abi_version: TYPED_GC_ABI_VERSION,
        flags: 0,
        fixed_size: TEXT_OBJECT_HEADER_SIZE,
        object_align: align_of::<ByteObject>() as u64,
        pointer_count: 0,
        pointer_offsets: ptr::null(),
    };
    let mut allocated = ptr::null_mut();
    // SAFETY: metadata and the local unpublished output cell remain stable for
    // the complete allocation, including any moving collection.
    let status = unsafe {
        allocate_typed_object(&raw const descriptor, allocation_size, &raw mut allocated)
    };
    if status == GC_RESOURCE_LIMIT {
        std::process::abort();
    }
    if status != GC_OK {
        return status;
    }
    let object = allocated.cast::<ByteObject>();
    // SAFETY: the fresh zeroed typed allocation has the exact fixed header and
    // trailing byte extent. Initialization contains no safepoint; publication
    // to the caller-owned cell is deliberately the final operation.
    unsafe {
        object.write(ByteObject {
            layout: &raw const BYTES_LAYOUT_DESCRIPTOR,
            allocation_size,
            byte_length,
            reserved: 0,
            bytes: [],
        });
        ptr::copy_nonoverlapping(
            staged.as_ptr(),
            allocated.cast::<u8>().add(TEXT_OBJECT_HEADER_BYTES),
            staged.len(),
        );
        output.write(allocated);
    }
    GC_OK
}

pub(crate) unsafe fn allocate_typed_text(
    staged: &[u8],
    scalar_length: u64,
    output: *mut *mut c_void,
) -> i32 {
    let Ok(byte_length) = u64::try_from(staged.len()) else {
        std::process::abort();
    };
    let Some(allocation_size) = TEXT_OBJECT_HEADER_SIZE.checked_add(byte_length) else {
        std::process::abort();
    };
    let descriptor = LoomGcObjectDescriptor {
        abi_version: TYPED_GC_ABI_VERSION,
        flags: 0,
        fixed_size: TEXT_OBJECT_HEADER_SIZE,
        object_align: align_of::<TextObject>() as u64,
        pointer_count: 0,
        pointer_offsets: ptr::null(),
    };
    let mut allocated = ptr::null_mut();
    // SAFETY: the descriptor is process-lifetime immutable metadata and the
    // local output cell remains stable for the complete allocation call.
    let status = unsafe {
        allocate_typed_object(&raw const descriptor, allocation_size, &raw mut allocated)
    };
    if status == GC_RESOURCE_LIMIT {
        std::process::abort();
    }
    if status != GC_OK {
        return status;
    }
    let object = allocated.cast::<TextObject>();
    // SAFETY: the typed allocator returned a zeroed allocation with the exact
    // header/alignment and enough pointer-free trailing storage. Initialization
    // performs no allocation or runtime call, so it contains no safepoint.
    unsafe {
        object.write(TextObject {
            layout: &raw const TEXT_LAYOUT_DESCRIPTOR,
            allocation_size,
            byte_length,
            scalar_length,
            bytes: [],
        });
        ptr::copy_nonoverlapping(
            staged.as_ptr(),
            allocated.cast::<u8>().add(TEXT_OBJECT_HEADER_BYTES),
            staged.len(),
        );
        output.write(allocated);
    }
    GC_OK
}

/// Publishes two independent managed Text values while keeping the first
/// allocation live across the second allocation's moving-GC safepoint.
///
/// The byte slices must remain stable for the complete call. Scheduler-owned
/// typed fault strings satisfy that contract because Task storage is not part
/// of the moving heap. Output cells are cleared on every recoverable failure.
pub(crate) unsafe fn allocate_typed_text_pair(
    first: &[u8],
    second: &[u8],
    first_output: *mut *mut c_void,
    second_output: *mut *mut c_void,
) -> i32 {
    let output_aligned = |output: *mut *mut c_void| {
        !output.is_null() && output.addr().is_multiple_of(align_of::<*mut c_void>())
    };
    if !output_aligned(first_output)
        || !output_aligned(second_output)
        || first_output == second_output
    {
        return GC_INVALID_ARGUMENT;
    }
    unsafe {
        first_output.write(ptr::null_mut());
        second_output.write(ptr::null_mut());
    }
    let (Ok(first_text), Ok(second_text)) =
        (std::str::from_utf8(first), std::str::from_utf8(second))
    else {
        return GC_INVALID_ARGUMENT;
    };
    let (Ok(first_scalars), Ok(second_scalars)) = (
        u64::try_from(first_text.chars().count()),
        u64::try_from(second_text.chars().count()),
    ) else {
        return GC_INVALID_ARGUMENT;
    };

    let live_bitmaps = [3_u64];
    let descriptor = LoomGcTypedRootDescriptor {
        abi_version: TYPED_SHADOW_STACK_ABI_VERSION,
        flags: 0,
        slot_count: 2,
        state_count: 1,
        live_bitmap_words: 1,
        live_bitmaps: live_bitmaps.as_ptr(),
    };
    let slots = [
        first_output.cast::<c_void>(),
        second_output.cast::<c_void>(),
    ];
    let mut frame = LoomGcTypedRootFrame {
        abi_version: TYPED_SHADOW_STACK_ABI_VERSION,
        flags: 0,
        state: 0,
        descriptor: &raw const descriptor,
        slots: slots.as_ptr(),
        previous: ptr::null_mut(),
    };
    let push_status = unsafe { typed_root_push_v1(&raw mut frame) };
    if push_status != GC_OK {
        return push_status;
    }

    let first_status = unsafe { allocate_typed_text(first, first_scalars, first_output) };
    let status = if first_status == GC_OK {
        unsafe { allocate_typed_text(second, second_scalars, second_output) }
    } else {
        first_status
    };
    if unsafe { typed_root_pop_v1(&raw mut frame) } != GC_OK {
        // Returning would leave the active root chain pointing into this stack
        // frame, so a root protocol defect is necessarily process-fatal.
        std::process::abort();
    }
    if status != GC_OK {
        unsafe {
            first_output.write(ptr::null_mut());
            second_output.write(ptr::null_mut());
        }
    }
    status
}

/// Allocates a managed Text object after validating UTF-8 and caching its
/// Unicode scalar length.
pub(crate) fn allocate_text_storage(bytes: &[u8]) -> Option<(Box<[u64]>, *mut TextObject)> {
    let text = std::str::from_utf8(bytes).ok()?;
    let scalar_length = u64::try_from(text.chars().count()).ok()?;
    let (allocation, object) = allocate_storage(bytes)?;
    let object = object.cast::<TextObject>();
    // SAFETY: allocate_storage reserved and aligned the complete header.
    unsafe {
        object.write(TextObject {
            layout: &raw const TEXT_LAYOUT_DESCRIPTOR,
            allocation_size: TEXT_OBJECT_HEADER_SIZE
                .checked_add(u64::try_from(bytes.len()).ok()?)?,
            byte_length: u64::try_from(bytes.len()).ok()?,
            scalar_length,
            bytes: [],
        });
    }
    Some((allocation, object))
}

pub(crate) fn allocate_byte_storage(bytes: &[u8]) -> Option<(Box<[u64]>, *mut ByteObject)> {
    let (allocation, object) = allocate_storage(bytes)?;
    let object = object.cast::<ByteObject>();
    // SAFETY: allocate_storage reserved and aligned the complete header.
    unsafe {
        object.write(ByteObject {
            layout: &raw const BYTES_LAYOUT_DESCRIPTOR,
            allocation_size: TEXT_OBJECT_HEADER_SIZE
                .checked_add(u64::try_from(bytes.len()).ok()?)?,
            byte_length: u64::try_from(bytes.len()).ok()?,
            reserved: 0,
            bytes: [],
        });
    }
    Some((allocation, object))
}

fn allocate_storage(bytes: &[u8]) -> Option<(Box<[u64]>, *mut u64)> {
    let allocation_size = TEXT_OBJECT_HEADER_BYTES.checked_add(bytes.len())?;
    let word_count = allocation_size.checked_add(size_of::<u64>() - 1)? / size_of::<u64>();
    let mut allocation = vec![0_u64; word_count.max(TEXT_OBJECT_HEADER_WORDS)].into_boxed_slice();
    let object = allocation.as_mut_ptr();
    // SAFETY: the u64 allocation contains the complete fixed header plus the
    // requested writable trailing byte range.
    unsafe {
        ptr::copy_nonoverlapping(
            bytes.as_ptr(),
            object.cast::<u8>().add(TEXT_OBJECT_HEADER_BYTES),
            bytes.len(),
        );
    }
    Some((allocation, object))
}

pub(crate) fn value(object: *mut c_void) -> ValueSlot {
    let mut slot = ValueSlot {
        words: [0; VALUE_SLOT_WORDS],
    };
    slot.words[VALUE_WORD_TAG] = VALUE_TAG_TEXT;
    slot.words[VALUE_WORD_DATA] = object as u64;
    slot
}

pub(crate) fn object(value: &ValueSlot) -> Option<*mut c_void> {
    if value.words[VALUE_WORD_TAG] != VALUE_TAG_TEXT
        || value.words[VALUE_WORD_NOMINAL] != 0
        || value.words[VALUE_WORD_AUX] != 0
        || value.words[VALUE_WORD_SCALAR] != 0
        || value.words[VALUE_WORD_WITNESS] != 0
        || value.words[VALUE_WORD_DATA] == 0
    {
        return None;
    }
    Some(value.words[VALUE_WORD_DATA] as *mut c_void)
}

pub(crate) unsafe fn bytes<'object>(object: *const c_void) -> Option<&'object [u8]> {
    if object.is_null() || !object.addr().is_multiple_of(align_of::<TextObject>()) {
        return None;
    }
    let header = unsafe { object.cast::<TextObject>().as_ref() }?;
    let expected = if header.layout == &raw const TEXT_LAYOUT_DESCRIPTOR {
        &TEXT_LAYOUT_DESCRIPTOR
    } else if header.layout == &raw const BYTES_LAYOUT_DESCRIPTOR {
        &BYTES_LAYOUT_DESCRIPTOR
    } else {
        return None;
    };
    let layout = unsafe { header.layout.as_ref() }?;
    let expected_size = TEXT_OBJECT_HEADER_SIZE.checked_add(header.byte_length)?;
    if layout != expected
        || layout.abi_version != LAYOUT_ABI_VERSION
        || !matches!(layout.kind, LAYOUT_KIND_TEXT | LAYOUT_KIND_BYTES)
        || layout.value_size != size_of::<*const c_void>() as u64
        || layout.value_align != align_of::<*const c_void>() as u64
        || layout.object_header_size != TEXT_OBJECT_HEADER_SIZE
        || layout.object_align != align_of::<TextObject>() as u64
        || layout.flags
            != (LAYOUT_FLAG_MANAGED_POINTER | LAYOUT_FLAG_LEAF | LAYOUT_FLAG_TRAILING_BYTES)
        || layout.reserved != 0
        || header.allocation_size != expected_size
    {
        return None;
    }
    if expected.kind == LAYOUT_KIND_BYTES
        && unsafe { object.cast::<ByteObject>().as_ref() }?.reserved != 0
    {
        return None;
    }
    let length = usize::try_from(header.byte_length).ok()?;
    // SAFETY: the validated allocation header promises exactly this readable
    // trailing byte range, and the caller keeps the managed object live.
    Some(unsafe {
        slice::from_raw_parts(object.cast::<u8>().add(TEXT_OBJECT_HEADER_BYTES), length)
    })
}

pub(crate) unsafe fn text_bytes<'object>(object: *const c_void) -> Option<&'object [u8]> {
    let bytes = unsafe { bytes(object) }?;
    let header = unsafe { object.cast::<TextObject>().as_ref() }?;
    if header.layout != &raw const TEXT_LAYOUT_DESCRIPTOR {
        return None;
    }
    Some(bytes)
}

#[cfg(test)]
unsafe fn byte_sequence_bytes<'object>(object: *const c_void) -> Option<&'object [u8]> {
    let bytes = unsafe { bytes(object) }?;
    let header = unsafe { object.cast::<ByteObject>().as_ref() }?;
    if header.layout != &raw const BYTES_LAYOUT_DESCRIPTOR {
        return None;
    }
    Some(bytes)
}

#[cfg(test)]
pub(crate) unsafe fn value_bytes(value: &ValueSlot) -> Option<&[u8]> {
    unsafe { bytes(object(value)?) }
}

pub(crate) unsafe fn text_value_bytes(value: &ValueSlot) -> Option<&[u8]> {
    unsafe { text_bytes(object(value)?) }
}

#[cfg(test)]
pub(crate) unsafe fn byte_value_bytes(value: &ValueSlot) -> Option<&[u8]> {
    unsafe { byte_sequence_bytes(object(value)?) }
}

pub(crate) unsafe fn scalar_length(object: *const TextObject) -> Option<u64> {
    unsafe { text_bytes(object.cast::<c_void>()) }?;
    let object = unsafe { object.as_ref() }?;
    Some(object.scalar_length)
}

#[cfg(test)]
unsafe fn validate_text_object_deep(object: *const TextObject) -> bool {
    let Some(bytes) = (unsafe { text_bytes(object.cast::<c_void>()) }) else {
        return false;
    };
    let Some(object) = (unsafe { object.as_ref() }) else {
        return false;
    };
    std::str::from_utf8(bytes)
        .is_ok_and(|text| u64::try_from(text.chars().count()).ok() == Some(object.scalar_length))
}

#[cfg(test)]
mod tests {
    use std::ffi::c_void;
    use std::mem::{align_of, offset_of, size_of};
    use std::ptr;

    use loom_runtime_abi::{
        BYTES_DECODE_UTF8_TYPED_INVALID_UTF8, GC_INVALID_ARGUMENT, GC_OK,
        LoomGcTypedRootDescriptor, LoomGcTypedRootFrame, LoomLayoutDescriptor,
        TEXT_GET_TYPED_FOUND, TEXT_GET_TYPED_INVALID, TEXT_GET_TYPED_MISSING,
        TEXT_OBJECT_ALIGNMENT, TEXT_OBJECT_HEADER_SIZE, TYPED_SHADOW_STACK_ABI_VERSION,
        VALUE_SLOT_WORDS, VALUE_TAG_TEXT,
    };

    use super::{
        BYTES_LAYOUT_DESCRIPTOR, ByteObject, TEXT_LAYOUT_DESCRIPTOR, TextObject,
        allocate_byte_storage, allocate_text_storage, bytes, bytes_append_typed_v1,
        bytes_decode_utf8_typed_v1, concat_typed_v1, get_typed_v1, scalar_length, text_bytes,
        validate_text_object_deep,
    };
    use crate::gc::{
        activate_runtime_v1, deactivate_runtime_v1, typed_root_pop_v1, typed_root_push_v1,
    };
    use crate::runtime::{runtime_create_v1, runtime_destroy_v1};
    use crate::scheduler::ValueSlot;

    #[test]
    fn host_value_and_text_layout_match_the_versioned_abi() {
        assert_eq!(
            size_of::<usize>(),
            8,
            "native Value ABI requires 64-bit pointers"
        );
        assert_eq!(size_of::<ValueSlot>(), VALUE_SLOT_WORDS * size_of::<u64>());
        assert_eq!(align_of::<ValueSlot>(), align_of::<u64>());
        assert_eq!(offset_of!(ValueSlot, words), 0);

        assert_eq!(size_of::<LoomLayoutDescriptor>(), 48);
        assert_eq!(align_of::<LoomLayoutDescriptor>(), 8);
        assert_eq!(size_of::<TextObject>() as u64, TEXT_OBJECT_HEADER_SIZE);
        assert_eq!(align_of::<TextObject>() as u64, TEXT_OBJECT_ALIGNMENT);
        assert_eq!(offset_of!(TextObject, layout), 0);
        assert_eq!(offset_of!(TextObject, allocation_size), 8);
        assert_eq!(offset_of!(TextObject, byte_length), 16);
        assert_eq!(offset_of!(TextObject, scalar_length), 24);
        assert_eq!(offset_of!(TextObject, bytes), 32);
        assert_eq!(TEXT_LAYOUT_DESCRIPTOR.value_size, 8);
        assert_eq!(TEXT_LAYOUT_DESCRIPTOR.value_align, 8);
        assert_eq!(TEXT_LAYOUT_DESCRIPTOR.object_header_size, 32);
        assert_eq!(TEXT_LAYOUT_DESCRIPTOR.object_align, 8);
        assert_eq!(size_of::<ByteObject>() as u64, TEXT_OBJECT_HEADER_SIZE);
        assert_eq!(offset_of!(ByteObject, bytes), 32);
        assert_ne!(
            &raw const TEXT_LAYOUT_DESCRIPTOR,
            &raw const BYTES_LAYOUT_DESCRIPTOR
        );
    }

    #[test]
    fn text_storage_is_inline_utf8_with_cached_scalar_length() {
        let (allocation, object) = allocate_text_storage("a界🙂".as_bytes()).unwrap();
        assert_eq!(unsafe { bytes(object.cast()) }, Some("a界🙂".as_bytes()));
        assert_eq!(unsafe { scalar_length(object) }, Some(3));
        assert!(unsafe { validate_text_object_deep(object) });
        drop(allocation);

        assert!(allocate_text_storage(&[0xff]).is_none());
        let (allocation, object) = allocate_byte_storage(&[0xff]).unwrap();
        assert_eq!(unsafe { bytes(object.cast()) }, Some(&[0xff][..]));
        assert_eq!(
            unsafe { (*object).layout },
            &raw const BYTES_LAYOUT_DESCRIPTOR
        );
        drop(allocation);
    }

    #[test]
    fn typed_concat_stages_aliases_before_forced_collection_and_publishes_last() {
        let (left_storage, left) = allocate_text_storage("a界".as_bytes()).unwrap();
        let (right_storage, right) = allocate_text_storage("🙂".as_bytes()).unwrap();
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        unsafe {
            assert_eq!(activate_runtime_v1(runtime), GC_OK);
            (*runtime).heap.collect_before_every_allocation = true;

            let bitmaps = [0_u64, 1_u64];
            let descriptor = LoomGcTypedRootDescriptor {
                abi_version: TYPED_SHADOW_STACK_ABI_VERSION,
                flags: 0,
                slot_count: 1,
                state_count: 2,
                live_bitmap_words: 1,
                live_bitmaps: bitmaps.as_ptr(),
            };
            let mut cell: *mut c_void = ptr::null_mut();
            let slots = [(&raw mut cell).cast::<c_void>()];
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
                concat_typed_v1(left.cast(), right.cast(), &raw mut cell),
                GC_OK
            );
            assert_eq!(text_bytes(cell), Some("a界🙂".as_bytes()));
            let first = cell;
            assert_eq!(concat_typed_v1(first, first, &raw mut cell), GC_OK);
            assert_eq!(text_bytes(cell), Some("a界🙂a界🙂".as_bytes()));
            assert!((*runtime).heap.collections >= 2);
            assert!((*runtime).heap.reclaimed >= 1);

            assert_eq!(typed_root_pop_v1(&raw mut frame), GC_OK);
            (*runtime).heap.collect_before_every_allocation = false;
            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
        drop(right_storage);
        drop(left_storage);
    }

    #[test]
    fn typed_get_stages_one_scalar_and_has_a_nonallocating_missing_path() {
        let (source_storage, source) = allocate_text_storage("a界🙂".as_bytes()).unwrap();
        let (empty_storage, empty) = allocate_text_storage(b"").unwrap();
        let (bytes_storage, bytes) = allocate_byte_storage("bytes".as_bytes()).unwrap();
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        unsafe {
            assert_eq!(activate_runtime_v1(runtime), GC_OK);
            (*runtime).heap.collect_before_every_allocation = true;

            let bitmaps = [0_u64, 1_u64];
            let descriptor = LoomGcTypedRootDescriptor {
                abi_version: TYPED_SHADOW_STACK_ABI_VERSION,
                flags: 0,
                slot_count: 1,
                state_count: 2,
                live_bitmap_words: 1,
                live_bitmaps: bitmaps.as_ptr(),
            };
            let mut cell: *mut c_void = ptr::null_mut();
            let slots = [(&raw mut cell).cast::<c_void>()];
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
                concat_typed_v1(source.cast(), empty.cast(), &raw mut cell),
                GC_OK
            );
            let managed_source = cell;
            assert_eq!(text_bytes(cell), Some("a界🙂".as_bytes()));
            assert_eq!(get_typed_v1(cell, 1, &raw mut cell), TEXT_GET_TYPED_FOUND);
            assert_eq!(text_bytes(cell), Some("界".as_bytes()));
            // Addresses are deliberately unobservable and the allocator may
            // reuse the reclaimed source address for this fresh result.
            let _ = managed_source;
            assert!((*runtime).heap.reclaimed >= 1);
            let collections = (*runtime).heap.collections;
            let result = cell;

            let mut missing = ptr::dangling_mut::<c_void>();
            assert_eq!(
                get_typed_v1(cell, -1, &raw mut missing),
                TEXT_GET_TYPED_MISSING,
            );
            assert!(missing.is_null());
            assert_eq!(
                get_typed_v1(cell, 1, &raw mut missing),
                TEXT_GET_TYPED_MISSING,
            );
            assert_eq!((*runtime).heap.collections, collections);
            assert_eq!(cell, result);

            let mut invalid = ptr::dangling_mut::<c_void>();
            assert_eq!(
                get_typed_v1(bytes.cast(), 0, &raw mut invalid),
                TEXT_GET_TYPED_INVALID,
            );
            assert!(invalid.is_null());
            assert_eq!(typed_root_pop_v1(&raw mut frame), GC_OK);
            (*runtime).heap.collect_before_every_allocation = false;
            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);

            invalid = ptr::dangling_mut();
            assert_eq!(
                get_typed_v1(source.cast(), 0, &raw mut invalid),
                TEXT_GET_TYPED_INVALID,
            );
            assert!(invalid.is_null());
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
        drop(bytes_storage);
        drop(empty_storage);
        drop(source_storage);
    }

    #[test]
    fn typed_bytes_append_stages_both_layouts_before_forced_collection() {
        let (left_storage, left) = allocate_text_storage(b"left\0").unwrap();
        let (right_storage, right) = allocate_byte_storage(&[0xff, 2]).unwrap();
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        unsafe {
            assert_eq!(activate_runtime_v1(runtime), GC_OK);
            (*runtime).heap.collect_before_every_allocation = true;

            let bitmaps = [0_u64, 1_u64];
            let descriptor = LoomGcTypedRootDescriptor {
                abi_version: TYPED_SHADOW_STACK_ABI_VERSION,
                flags: 0,
                slot_count: 1,
                state_count: 2,
                live_bitmap_words: 1,
                live_bitmaps: bitmaps.as_ptr(),
            };
            let mut cell: *mut c_void = ptr::null_mut();
            let slots = [(&raw mut cell).cast::<c_void>()];
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
                bytes_append_typed_v1(left.cast(), right.cast(), &raw mut cell),
                GC_OK
            );
            assert_eq!(bytes(cell), Some(&b"left\0\xff\x02"[..]));
            assert_eq!(
                (*cell.cast::<ByteObject>()).layout,
                &raw const BYTES_LAYOUT_DESCRIPTOR
            );

            let first = cell;
            assert_eq!(bytes_append_typed_v1(first, first, &raw mut cell), GC_OK);
            assert_eq!(bytes(cell), Some(&b"left\0\xff\x02left\0\xff\x02"[..]));
            assert!((*runtime).heap.collections >= 2);
            assert!((*runtime).heap.reclaimed >= 1);

            assert_eq!(typed_root_pop_v1(&raw mut frame), GC_OK);
            (*runtime).heap.collect_before_every_allocation = false;
            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
        drop(right_storage);
        drop(left_storage);
    }

    #[test]
    fn typed_bytes_decode_has_distinct_utf8_and_defect_statuses() {
        let (valid_storage, valid) = allocate_byte_storage("a界🙂".as_bytes()).unwrap();
        let (shared_text_storage, shared_text) =
            allocate_text_storage("shared 界🙂".as_bytes()).unwrap();
        let (malformed_text_storage, malformed_text) =
            allocate_text_storage("bad cache".as_bytes()).unwrap();
        unsafe { (*malformed_text).scalar_length += 1 };
        let (empty_storage, empty) = allocate_text_storage(b"").unwrap();
        let (invalid_storage, invalid) = allocate_byte_storage(&[0xff]).unwrap();
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        unsafe {
            assert_eq!(activate_runtime_v1(runtime), GC_OK);
            (*runtime).heap.collect_before_every_allocation = true;

            let bitmaps = [0_u64, 1_u64];
            let descriptor = LoomGcTypedRootDescriptor {
                abi_version: TYPED_SHADOW_STACK_ABI_VERSION,
                flags: 0,
                slot_count: 1,
                state_count: 2,
                live_bitmap_words: 1,
                live_bitmaps: bitmaps.as_ptr(),
            };
            let mut cell: *mut c_void = ptr::null_mut();
            let slots = [(&raw mut cell).cast::<c_void>()];
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
                bytes_append_typed_v1(valid.cast(), empty.cast(), &raw mut cell),
                GC_OK
            );
            assert_eq!(bytes(cell), Some("a界🙂".as_bytes()));
            assert_eq!(bytes_decode_utf8_typed_v1(cell, &raw mut cell), GC_OK);
            assert_eq!(text_bytes(cell), Some("a界🙂".as_bytes()));
            assert_eq!(scalar_length(cell.cast()), Some(3));
            assert!((*runtime).heap.reclaimed >= 1);

            let collections = (*runtime).heap.collections;
            let mut shared: *mut c_void = shared_text.cast();
            assert_eq!(bytes_decode_utf8_typed_v1(shared, &raw mut shared), GC_OK);
            assert_eq!(shared, shared_text.cast());
            assert_eq!(text_bytes(shared), Some("shared 界🙂".as_bytes()));
            assert_eq!(scalar_length(shared.cast()), Some(9));
            assert_eq!((*runtime).heap.collections, collections);

            let mut failed = ptr::dangling_mut::<c_void>();
            assert_eq!(
                bytes_decode_utf8_typed_v1(malformed_text.cast(), &raw mut failed),
                GC_INVALID_ARGUMENT
            );
            assert!(failed.is_null());
            assert_eq!((*runtime).heap.collections, collections);

            failed = ptr::dangling_mut();
            assert_eq!(
                bytes_decode_utf8_typed_v1(invalid.cast(), &raw mut failed),
                BYTES_DECODE_UTF8_TYPED_INVALID_UTF8
            );
            assert!(failed.is_null());
            assert_eq!((*runtime).heap.collections, collections);

            failed = ptr::dangling_mut();
            assert_eq!(
                bytes_decode_utf8_typed_v1(ptr::null(), &raw mut failed),
                GC_INVALID_ARGUMENT
            );
            assert!(failed.is_null());
            failed = ptr::dangling_mut();
            assert_eq!(
                bytes_append_typed_v1(ptr::null(), valid.cast(), &raw mut failed),
                GC_INVALID_ARGUMENT
            );
            assert!(failed.is_null());

            assert_eq!(typed_root_pop_v1(&raw mut frame), GC_OK);
            (*runtime).heap.collect_before_every_allocation = false;
            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);

            failed = ptr::dangling_mut();
            assert_eq!(
                bytes_decode_utf8_typed_v1(valid.cast(), &raw mut failed),
                GC_INVALID_ARGUMENT
            );
            assert!(failed.is_null());
            failed = ptr::dangling_mut();
            assert_eq!(
                bytes_append_typed_v1(valid.cast(), empty.cast(), &raw mut failed),
                GC_INVALID_ARGUMENT
            );
            assert!(failed.is_null());
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
        drop(invalid_storage);
        drop(empty_storage);
        drop(malformed_text_storage);
        drop(shared_text_storage);
        drop(valid_storage);
    }

    #[test]
    fn malformed_envelopes_and_headers_fail_closed() {
        let (allocation, object) = allocate_text_storage(b"safe").unwrap();
        let value = super::value(object.cast());
        assert_eq!(value.words, [VALUE_TAG_TEXT, 0, 0, 0, object as u64, 0]);
        assert_eq!(
            unsafe { super::text_value_bytes(&value) },
            Some(&b"safe"[..])
        );
        for index in [
            loom_runtime_abi::VALUE_WORD_NOMINAL,
            loom_runtime_abi::VALUE_WORD_AUX,
            loom_runtime_abi::VALUE_WORD_SCALAR,
            loom_runtime_abi::VALUE_WORD_WITNESS,
        ] {
            let mut dirty = value;
            dirty.words[index] = 1;
            assert_eq!(unsafe { super::text_value_bytes(&dirty) }, None);
        }
        let mut missing = value;
        missing.words[loom_runtime_abi::VALUE_WORD_DATA] = 0;
        assert_eq!(unsafe { super::text_value_bytes(&missing) }, None);
        assert_eq!(unsafe { bytes(object.cast::<u8>().add(1).cast()) }, None);
        drop(allocation);

        let (allocation, object) = allocate_text_storage(b"safe").unwrap();
        // SAFETY: this test owns the complete allocation and deliberately
        // corrupts one header field to exercise fail-closed validation.
        unsafe { (*object).allocation_size += 1 };
        assert_eq!(unsafe { bytes(object.cast()) }, None);
        assert_eq!(unsafe { scalar_length(object) }, None);
        drop(allocation);

        let (allocation, object) = allocate_text_storage(b"safe").unwrap();
        let forged = TEXT_LAYOUT_DESCRIPTOR;
        // SAFETY: this test owns the object and substitutes an equal-by-value
        // descriptor at a different address. Descriptor identity must be exact.
        unsafe { (*object).layout = &raw const forged };
        assert_eq!(unsafe { bytes(object.cast()) }, None);
        drop(allocation);

        let (allocation, object) = allocate_text_storage("界".as_bytes()).unwrap();
        // SAFETY: this test owns the object and uses deep validation to prove
        // producer invariants include the cached scalar count.
        unsafe { (*object).scalar_length = 2 };
        assert!(!unsafe { validate_text_object_deep(object) });
        drop(allocation);

        let (allocation, object) = allocate_byte_storage(b"bytes").unwrap();
        // SAFETY: this test owns the ByteObject and corrupts its required-zero
        // reserved header field.
        unsafe { (*object).reserved = 1 };
        assert_eq!(unsafe { bytes(object.cast()) }, None);
        drop(allocation);
    }
}
