//! Managed immutable storage used by native `Text` compatibility envelopes.

use std::ffi::c_void;
use std::mem::{align_of, size_of};
use std::ptr;
use std::slice;

use loom_runtime_abi::{
    LAYOUT_ABI_VERSION, LAYOUT_FLAG_LEAF, LAYOUT_FLAG_MANAGED_POINTER, LAYOUT_FLAG_TRAILING_BYTES,
    LAYOUT_KIND_BYTES, LAYOUT_KIND_TEXT, LoomLayoutDescriptor, TEXT_OBJECT_HEADER_SIZE,
    VALUE_SLOT_WORDS, VALUE_TAG_TEXT, VALUE_WORD_AUX, VALUE_WORD_DATA, VALUE_WORD_NOMINAL,
    VALUE_WORD_SCALAR, VALUE_WORD_TAG, VALUE_WORD_WITNESS,
};

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

const TEXT_OBJECT_HEADER_WORDS: usize = TEXT_OBJECT_HEADER_SIZE as usize / size_of::<u64>();

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
            allocation_size: u64::try_from(TEXT_OBJECT_HEADER_SIZE as usize + bytes.len()).ok()?,
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
            allocation_size: u64::try_from(TEXT_OBJECT_HEADER_SIZE as usize + bytes.len()).ok()?,
            byte_length: u64::try_from(bytes.len()).ok()?,
            reserved: 0,
            bytes: [],
        });
    }
    Some((allocation, object))
}

fn allocate_storage(bytes: &[u8]) -> Option<(Box<[u64]>, *mut u8)> {
    let allocation_size = (TEXT_OBJECT_HEADER_SIZE as usize).checked_add(bytes.len())?;
    let word_count = allocation_size.checked_add(size_of::<u64>() - 1)? / size_of::<u64>();
    let mut allocation = vec![0_u64; word_count.max(TEXT_OBJECT_HEADER_WORDS)].into_boxed_slice();
    let object = allocation.as_mut_ptr().cast::<u8>();
    // SAFETY: the u64 allocation contains the complete fixed header plus the
    // requested writable trailing byte range.
    unsafe {
        ptr::copy_nonoverlapping(
            bytes.as_ptr(),
            object.add(TEXT_OBJECT_HEADER_SIZE as usize),
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
        || object.addr() % align_of::<TextObject>() != 0
    {
        return None;
    }
    let length = usize::try_from(header.byte_length).ok()?;
    // SAFETY: the validated allocation header promises exactly this readable
    // trailing byte range, and the caller keeps the managed object live.
    Some(unsafe {
        slice::from_raw_parts(
            object.cast::<u8>().add(TEXT_OBJECT_HEADER_SIZE as usize),
            length,
        )
    })
}

pub(crate) unsafe fn value_bytes(value: &ValueSlot) -> Option<&[u8]> {
    unsafe { bytes(object(value)?) }
}

pub(crate) unsafe fn scalar_length(object: *const TextObject) -> Option<u64> {
    let object = unsafe { object.as_ref() }?;
    if object.layout != &raw const TEXT_LAYOUT_DESCRIPTOR {
        return None;
    }
    Some(object.scalar_length)
}

#[cfg(test)]
mod tests {
    use std::mem::{align_of, offset_of, size_of};

    use loom_runtime_abi::{
        LoomLayoutDescriptor, TEXT_OBJECT_ALIGNMENT, TEXT_OBJECT_HEADER_SIZE, VALUE_SLOT_WORDS,
    };

    use super::{
        BYTES_LAYOUT_DESCRIPTOR, ByteObject, TEXT_LAYOUT_DESCRIPTOR, TextObject,
        allocate_byte_storage, allocate_text_storage, bytes, scalar_length,
    };
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
}
