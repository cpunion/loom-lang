//! Canonical JSON formatting for compiler-shaped direct values.
//!
//! This boundary deliberately does not decode or construct the universal
//! `ValueSlot`. Generated code supplies one target-data descriptor,
//! and the runtime reads only the closed direct Json/List/TextMap shapes named
//! by that descriptor. All input is consumed into ordinary Rust staging
//! storage before the sole managed Text allocation.

use std::ffi::c_void;
use std::mem::MaybeUninit;
use std::mem::{align_of, size_of};
use std::ptr;

use loom_runtime_abi::{
    GC_MAX_OBJECT_ALIGNMENT, GC_MAX_OBJECT_BYTES, GC_OK, GC_RESOURCE_LIMIT, LoomTypedJsonLayout,
    TEXT_OBJECT_HEADER_SIZE, TYPED_JSON_ABI_VERSION, TYPED_JSON_FORMAT_ABI_MISMATCH,
    TYPED_JSON_FORMAT_DEPTH_LIMIT, TYPED_JSON_FORMAT_DESCRIPTOR_INVALID,
    TYPED_JSON_FORMAT_INVALID_ARGUMENT, TYPED_JSON_FORMAT_NON_FINITE_NUMBER, TYPED_JSON_FORMAT_OK,
    TYPED_JSON_FORMAT_RESOURCE_LIMIT,
};

use crate::standard::JSON_DEPTH_LIMIT;
use crate::text;

const JSON_TAG_NULL: u32 = 0;
const JSON_TAG_BOOL: u32 = 1;
const JSON_TAG_NUMBER: u32 = 2;
const JSON_TAG_TEXT: u32 = 3;
const JSON_TAG_ARRAY: u32 = 4;
const JSON_TAG_OBJECT: u32 = 5;

#[derive(Clone, Copy)]
struct ValidatedLayout {
    raw: LoomTypedJsonLayout,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FormatFailure {
    InvalidValue,
    DepthLimit,
    NonFiniteNumber,
    ResourceLimit,
}

impl FormatFailure {
    const fn status(self) -> i32 {
        match self {
            Self::InvalidValue => TYPED_JSON_FORMAT_INVALID_ARGUMENT,
            Self::DepthLimit => TYPED_JSON_FORMAT_DEPTH_LIMIT,
            Self::NonFiniteNumber => TYPED_JSON_FORMAT_NON_FINITE_NUMBER,
            Self::ResourceLimit => TYPED_JSON_FORMAT_RESOURCE_LIMIT,
        }
    }
}

struct JsonStage {
    output: String,
    maximum_bytes: usize,
}

impl JsonStage {
    fn for_object_limit(object_limit: u64) -> Result<Self, FormatFailure> {
        let maximum_bytes = Self::maximum_bytes_for_object_limit(object_limit)?;
        Ok(Self::with_maximum_bytes(maximum_bytes))
    }

    fn maximum_bytes_for_object_limit(object_limit: u64) -> Result<usize, FormatFailure> {
        object_limit
            .checked_sub(TEXT_OBJECT_HEADER_SIZE)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or(FormatFailure::ResourceLimit)
    }

    fn with_maximum_bytes(maximum_bytes: usize) -> Self {
        Self {
            output: String::new(),
            maximum_bytes,
        }
    }

    fn checked_output_length(&self, additional: usize) -> Result<usize, FormatFailure> {
        self.output
            .len()
            .checked_add(additional)
            .filter(|length| *length <= self.maximum_bytes)
            .ok_or(FormatFailure::ResourceLimit)
    }

    fn push_str(&mut self, value: &str) -> Result<(), FormatFailure> {
        let length = self.checked_output_length(value.len())?;
        self.output
            .try_reserve(length - self.output.len())
            .map_err(|_| FormatFailure::ResourceLimit)?;
        self.output.push_str(value);
        Ok(())
    }

    fn push_char(&mut self, value: char) -> Result<(), FormatFailure> {
        let mut encoded = [0_u8; 4];
        self.push_str(value.encode_utf8(&mut encoded))
    }

    fn push_escaped_text(&mut self, value: &str) -> Result<(), FormatFailure> {
        const HEX: &[u8; 16] = b"0123456789abcdef";

        self.push_str("\"")?;
        for character in value.chars() {
            match character {
                '"' => self.push_str("\\\"")?,
                '\\' => self.push_str("\\\\")?,
                '\u{08}' => self.push_str("\\b")?,
                '\u{0c}' => self.push_str("\\f")?,
                '\n' => self.push_str("\\n")?,
                '\r' => self.push_str("\\r")?,
                '\t' => self.push_str("\\t")?,
                control if control <= '\u{1f}' => {
                    let value = u32::from(control) as usize;
                    self.push_str("\\u00")?;
                    self.push_char(char::from(HEX[value >> 4]))?;
                    self.push_char(char::from(HEX[value & 0x0f]))?;
                }
                character => self.push_char(character)?,
            }
        }
        self.push_str("\"")
    }
}

fn checked_span(offset: u64, size: u64, limit: u64) -> bool {
    offset.checked_add(size).is_some_and(|end| end <= limit)
}

fn spans_overlap(left_offset: u64, left_size: u64, right_offset: u64, right_size: u64) -> bool {
    left_offset < right_offset.saturating_add(right_size)
        && right_offset < left_offset.saturating_add(left_size)
}

fn offset_is_aligned(offset: u64, alignment: u64) -> bool {
    alignment != 0 && offset.is_multiple_of(alignment)
}

fn descriptor_value_limit(layout: &LoomTypedJsonLayout) -> bool {
    [
        layout.json_size,
        layout.tag_offset,
        layout.bool_payload_offset,
        layout.number_payload_offset,
        layout.text_payload_offset,
        layout.array_payload_offset,
        layout.object_payload_offset,
        layout.list_length_offset,
        layout.list_capacity_offset,
        layout.list_data_offset,
        layout.list_element_stride,
        layout.map_length_offset,
        layout.map_data_offset,
        layout.map_entry_stride,
        layout.map_key_offset,
        layout.map_value_offset,
    ]
    .into_iter()
    .all(|value| value <= GC_MAX_OBJECT_BYTES && usize::try_from(value).is_ok())
}

#[expect(
    clippy::too_many_lines,
    reason = "the descriptor's cross-field validation is one atomic proof"
)]
fn validate_layout(layout: LoomTypedJsonLayout) -> Result<ValidatedLayout, i32> {
    if layout.abi_version != TYPED_JSON_ABI_VERSION {
        return Err(TYPED_JSON_FORMAT_ABI_MISMATCH);
    }
    if !descriptor_value_limit(&layout) {
        return Err(TYPED_JSON_FORMAT_RESOURCE_LIMIT);
    }

    let pointer_size = size_of::<*const c_void>() as u64;
    let pointer_alignment = align_of::<*const c_void>() as u64;
    let number_size = size_of::<f64>() as u64;
    let number_alignment = align_of::<f64>() as u64;
    if pointer_size != 8
        || layout.flags != 0
        || layout.json_size == 0
        || layout.json_alignment < pointer_alignment.max(number_alignment)
        || !layout.json_alignment.is_power_of_two()
        || layout.json_alignment > GC_MAX_OBJECT_ALIGNMENT
        || !layout.json_size.is_multiple_of(layout.json_alignment)
        || !matches!(layout.tag_size, 1 | 2 | 4)
        || !offset_is_aligned(layout.tag_offset, layout.tag_size)
        || !checked_span(layout.tag_offset, layout.tag_size, layout.json_size)
        || !checked_span(layout.bool_payload_offset, 1, layout.json_size)
        || !offset_is_aligned(layout.number_payload_offset, number_alignment)
        || !checked_span(layout.number_payload_offset, number_size, layout.json_size)
    {
        return Err(TYPED_JSON_FORMAT_DESCRIPTOR_INVALID);
    }

    let pointer_payloads = [
        layout.text_payload_offset,
        layout.array_payload_offset,
        layout.object_payload_offset,
    ];
    if pointer_payloads.iter().any(|offset| {
        !offset_is_aligned(*offset, pointer_alignment)
            || !checked_span(*offset, pointer_size, layout.json_size)
    }) {
        return Err(TYPED_JSON_FORMAT_DESCRIPTOR_INVALID);
    }
    let payload_spans = [
        (layout.bool_payload_offset, 1),
        (layout.number_payload_offset, number_size),
        (layout.text_payload_offset, pointer_size),
        (layout.array_payload_offset, pointer_size),
        (layout.object_payload_offset, pointer_size),
    ];
    if payload_spans
        .iter()
        .any(|(offset, size)| spans_overlap(layout.tag_offset, layout.tag_size, *offset, *size))
    {
        return Err(TYPED_JSON_FORMAT_DESCRIPTOR_INVALID);
    }

    let header_alignment = align_of::<u64>() as u64;
    let header_size = size_of::<u64>() as u64;
    if !offset_is_aligned(layout.list_length_offset, header_alignment)
        || !offset_is_aligned(layout.list_capacity_offset, header_alignment)
        || !checked_span(
            layout.list_length_offset,
            header_size,
            layout.list_data_offset,
        )
        || !checked_span(
            layout.list_capacity_offset,
            header_size,
            layout.list_data_offset,
        )
        || spans_overlap(
            layout.list_length_offset,
            header_size,
            layout.list_capacity_offset,
            header_size,
        )
        || !offset_is_aligned(layout.list_data_offset, layout.json_alignment)
        || layout.list_element_stride < layout.json_size
        || !layout
            .list_element_stride
            .is_multiple_of(layout.json_alignment)
        || !checked_span(
            layout.list_data_offset,
            layout.list_element_stride,
            GC_MAX_OBJECT_BYTES,
        )
    {
        return Err(TYPED_JSON_FORMAT_DESCRIPTOR_INVALID);
    }

    let entry_alignment = pointer_alignment.max(layout.json_alignment);
    if !offset_is_aligned(layout.map_length_offset, header_alignment)
        || !checked_span(
            layout.map_length_offset,
            header_size,
            layout.map_data_offset,
        )
        || !offset_is_aligned(layout.map_data_offset, entry_alignment)
        || !offset_is_aligned(layout.map_key_offset, pointer_alignment)
        || !checked_span(layout.map_key_offset, pointer_size, layout.map_entry_stride)
        || !offset_is_aligned(layout.map_value_offset, layout.json_alignment)
        || !checked_span(
            layout.map_value_offset,
            layout.json_size,
            layout.map_entry_stride,
        )
        || spans_overlap(
            layout.map_key_offset,
            pointer_size,
            layout.map_value_offset,
            layout.json_size,
        )
        || layout.map_entry_stride == 0
        || !layout.map_entry_stride.is_multiple_of(entry_alignment)
        || !checked_span(
            layout.map_data_offset,
            layout.map_entry_stride,
            GC_MAX_OBJECT_BYTES,
        )
    {
        return Err(TYPED_JSON_FORMAT_DESCRIPTOR_INVALID);
    }

    Ok(ValidatedLayout { raw: layout })
}

impl ValidatedLayout {
    fn offset(value: u64) -> usize {
        usize::try_from(value).expect("validated typed Json descriptor offset")
    }

    unsafe fn read_at<T: Copy>(base: *const u8, offset: u64) -> T {
        let mut value = MaybeUninit::<T>::uninit();
        // SAFETY: every caller has validated a readable field of exactly T's
        // size. Byte copying also preserves pointer provenance without making
        // a stricter-alignment cast from the byte-oriented object base.
        unsafe {
            ptr::copy_nonoverlapping(
                base.add(Self::offset(offset)),
                value.as_mut_ptr().cast::<u8>(),
                size_of::<T>(),
            );
            value.assume_init()
        }
    }

    unsafe fn read_u64(base: *const u8, offset: u64) -> u64 {
        unsafe { Self::read_at(base, offset) }
    }

    unsafe fn read_pointer(base: *const u8, offset: u64) -> *const c_void {
        unsafe { Self::read_at(base, offset) }
    }

    unsafe fn read_tag(self, json: *const u8) -> u32 {
        // SAFETY: descriptor validation proves the selected tag load is
        // aligned and wholly inside the direct Json value.
        let tag = unsafe { json.add(Self::offset(self.raw.tag_offset)) };
        match self.raw.tag_size {
            1 => u32::from(unsafe { Self::read_at::<u8>(tag, 0) }),
            2 => u32::from(unsafe { Self::read_at::<u16>(tag, 0) }),
            4 => unsafe { Self::read_at::<u32>(tag, 0) },
            _ => unreachable!("validated typed Json tag size"),
        }
    }

    fn object_bytes(fixed_size: u64, count: u64, stride: u64) -> Result<(), FormatFailure> {
        let bytes = count
            .checked_mul(stride)
            .and_then(|bytes| bytes.checked_add(fixed_size))
            .filter(|bytes| *bytes <= GC_MAX_OBJECT_BYTES)
            .ok_or(FormatFailure::ResourceLimit)?;
        usize::try_from(bytes).map_err(|_| FormatFailure::ResourceLimit)?;
        Ok(())
    }

    unsafe fn text<'value>(pointer: *const c_void) -> Result<&'value str, FormatFailure> {
        // SAFETY: the direct value contract keeps the immutable managed Text
        // live until this non-safepoint traversal finishes.
        let bytes = unsafe { text::text_bytes(pointer) }.ok_or(FormatFailure::InvalidValue)?;
        let value = std::str::from_utf8(bytes).map_err(|_| FormatFailure::InvalidValue)?;
        let scalar_length =
            unsafe { text::scalar_length(pointer.cast()) }.ok_or(FormatFailure::InvalidValue)?;
        if u64::try_from(value.chars().count()).ok() != Some(scalar_length) {
            return Err(FormatFailure::InvalidValue);
        }
        Ok(value)
    }

    unsafe fn format_value(
        self,
        json: *const u8,
        depth: usize,
        stage: &mut JsonStage,
    ) -> Result<(), FormatFailure> {
        if json.is_null()
            || !json
                .addr()
                .is_multiple_of(Self::offset(self.raw.json_alignment))
        {
            return Err(FormatFailure::InvalidValue);
        }
        match unsafe { self.read_tag(json) } {
            JSON_TAG_NULL => stage.push_str("null"),
            JSON_TAG_BOOL => {
                // SAFETY: the validated payload byte is inside `json`.
                let value = unsafe { json.add(Self::offset(self.raw.bool_payload_offset)).read() };
                match value {
                    0 => stage.push_str("false"),
                    1 => stage.push_str("true"),
                    _ => Err(FormatFailure::InvalidValue),
                }
            }
            JSON_TAG_NUMBER => {
                // SAFETY: descriptor validation proves aligned f64 storage.
                let value = unsafe { Self::read_at::<f64>(json, self.raw.number_payload_offset) };
                if !value.is_finite() {
                    return Err(FormatFailure::NonFiniteNumber);
                }
                stage.push_str(&value.to_string())
            }
            JSON_TAG_TEXT => {
                let pointer = unsafe { Self::read_pointer(json, self.raw.text_payload_offset) };
                let value = unsafe { Self::text(pointer) }?;
                stage.push_escaped_text(value)
            }
            JSON_TAG_ARRAY => {
                if depth >= JSON_DEPTH_LIMIT {
                    return Err(FormatFailure::DepthLimit);
                }
                let object = unsafe { Self::read_pointer(json, self.raw.array_payload_offset) };
                unsafe { self.format_array(object, depth, stage) }
            }
            JSON_TAG_OBJECT => {
                if depth >= JSON_DEPTH_LIMIT {
                    return Err(FormatFailure::DepthLimit);
                }
                let object = unsafe { Self::read_pointer(json, self.raw.object_payload_offset) };
                unsafe { self.format_object(object, depth, stage) }
            }
            _ => Err(FormatFailure::InvalidValue),
        }
    }

    unsafe fn format_array(
        self,
        object: *const c_void,
        depth: usize,
        stage: &mut JsonStage,
    ) -> Result<(), FormatFailure> {
        stage.push_str("[")?;
        if object.is_null() {
            return stage.push_str("]");
        }
        let object = object.cast::<u8>();
        let object_alignment = Self::offset(self.raw.json_alignment.max(align_of::<u64>() as u64));
        if !object.addr().is_multiple_of(object_alignment) {
            return Err(FormatFailure::InvalidValue);
        }
        let length = unsafe { Self::read_u64(object, self.raw.list_length_offset) };
        let capacity = unsafe { Self::read_u64(object, self.raw.list_capacity_offset) };
        if length == 0 || capacity == 0 || length > capacity {
            return Err(FormatFailure::InvalidValue);
        }
        Self::object_bytes(
            self.raw.list_data_offset,
            capacity,
            self.raw.list_element_stride,
        )?;
        for index in 0..length {
            if index != 0 {
                stage.push_str(",")?;
            }
            let offset = index
                .checked_mul(self.raw.list_element_stride)
                .and_then(|offset| offset.checked_add(self.raw.list_data_offset))
                .and_then(|offset| usize::try_from(offset).ok())
                .ok_or(FormatFailure::ResourceLimit)?;
            // SAFETY: the validated header and descriptor establish the
            // initialized element range and its exact stride.
            unsafe { self.format_value(object.add(offset), depth + 1, stage) }?;
        }
        stage.push_str("]")
    }

    unsafe fn format_object(
        self,
        object: *const c_void,
        depth: usize,
        stage: &mut JsonStage,
    ) -> Result<(), FormatFailure> {
        stage.push_str("{")?;
        if object.is_null() {
            return stage.push_str("}");
        }
        let object = object.cast::<u8>();
        let object_alignment = Self::offset(self.raw.json_alignment.max(align_of::<u64>() as u64));
        if !object.addr().is_multiple_of(object_alignment) {
            return Err(FormatFailure::InvalidValue);
        }
        let length = unsafe { Self::read_u64(object, self.raw.map_length_offset) };
        if length == 0 {
            return Err(FormatFailure::InvalidValue);
        }
        Self::object_bytes(self.raw.map_data_offset, length, self.raw.map_entry_stride)?;
        let mut previous_key: Option<&[u8]> = None;
        for index in 0..length {
            if index != 0 {
                stage.push_str(",")?;
            }
            let offset = index
                .checked_mul(self.raw.map_entry_stride)
                .and_then(|offset| offset.checked_add(self.raw.map_data_offset))
                .and_then(|offset| usize::try_from(offset).ok())
                .ok_or(FormatFailure::ResourceLimit)?;
            // SAFETY: the exact TextMap allocation contains this initialized
            // entry and descriptor validation proves both field spans.
            let entry = unsafe { object.add(offset) };
            let key_pointer = unsafe { Self::read_pointer(entry, self.raw.map_key_offset) };
            let key = unsafe { Self::text(key_pointer) }?;
            if previous_key.is_some_and(|previous| previous >= key.as_bytes()) {
                return Err(FormatFailure::InvalidValue);
            }
            previous_key = Some(key.as_bytes());
            stage.push_escaped_text(key)?;
            stage.push_str(":")?;
            // SAFETY: the map entry stores one complete direct Json value.
            unsafe {
                self.format_value(
                    entry.add(Self::offset(self.raw.map_value_offset)),
                    depth + 1,
                    stage,
                )
            }?;
        }
        stage.push_str("}")
    }
}

unsafe fn json_format_typed_with_object_limit(
    json: *const c_void,
    layout: *const LoomTypedJsonLayout,
    output: *mut *mut c_void,
    object_limit: u64,
) -> i32 {
    if json.is_null()
        || layout.is_null()
        || output.is_null()
        || !layout
            .addr()
            .is_multiple_of(align_of::<LoomTypedJsonLayout>())
        || !output.addr().is_multiple_of(align_of::<*mut c_void>())
    {
        return TYPED_JSON_FORMAT_INVALID_ARGUMENT;
    }
    // SAFETY: the caller contract requires one readable, naturally aligned
    // descriptor. Copying it prevents later validation from observing mutable
    // metadata.
    let layout = unsafe { layout.read() };
    let layout = match validate_layout(layout) {
        Ok(layout) => layout,
        Err(status) => return status,
    };
    if !json
        .addr()
        .is_multiple_of(ValidatedLayout::offset(layout.raw.json_alignment))
    {
        return TYPED_JSON_FORMAT_INVALID_ARGUMENT;
    }

    let mut stage = match JsonStage::for_object_limit(object_limit) {
        Ok(stage) => stage,
        Err(error) => return error.status(),
    };
    if let Err(error) = unsafe { layout.format_value(json.cast(), 0, &mut stage) } {
        return error.status();
    }
    let Ok(scalar_length) = u64::try_from(stage.output.chars().count()) else {
        return TYPED_JSON_FORMAT_RESOURCE_LIMIT;
    };
    // SAFETY: all direct inputs have been consumed into `stage`; this is the
    // function's sole Loom allocation/safepoint and publishes only a complete
    // canonical Text object. `allocate_typed_text` treats allocator resource
    // exhaustion as the process-level OOM policy, so its GC_RESOURCE_LIMIT arm
    // is defensive rather than a recoverable allocation path here.
    match unsafe { text::allocate_typed_text(stage.output.as_bytes(), scalar_length, output) } {
        GC_OK => TYPED_JSON_FORMAT_OK,
        GC_RESOURCE_LIMIT => TYPED_JSON_FORMAT_RESOURCE_LIMIT,
        _ => TYPED_JSON_FORMAT_INVALID_ARGUMENT,
    }
}

/// Formats one direct Json value into a freshly allocated direct Text.
///
/// The output cell is written only after the complete graph has been
/// validated and staged. Returning any nonzero status leaves it untouched.
///
/// # Safety
///
/// `layout` must point to one readable, naturally aligned descriptor emitted
/// for `json`. `json` must point to a complete, naturally aligned direct Json
/// value, and every recursively reachable `Text`, `List`, and `TextMap` object must
/// remain live, immutable, and readable for the complete staging traversal.
/// This trusted compiler/runtime ABI does not establish allocation provenance
/// for arbitrary foreign pointers.
///
/// `output` must point to writable, pointer-aligned storage which does not
/// overlap the input graph or descriptor and whose address remains stable for
/// the complete call, including the final moving-GC allocation. In particular,
/// the output cell must not reside in either moving heap. A runtime must be
/// active when formatting succeeds and reaches that allocation.
#[unsafe(export_name = "loom_runtime_json_format_typed_v1")]
pub unsafe extern "C" fn json_format_typed_v1(
    json: *const c_void,
    layout: *const LoomTypedJsonLayout,
    output: *mut *mut c_void,
) -> i32 {
    unsafe { json_format_typed_with_object_limit(json, layout, output, GC_MAX_OBJECT_BYTES) }
}

#[cfg(test)]
mod tests {
    use std::ffi::c_void;
    use std::mem::{align_of, offset_of, size_of};
    use std::ptr;

    use loom_runtime_abi::{
        GC_MAX_OBJECT_BYTES, GC_OK, LoomTypedJsonLayout, TYPED_JSON_ABI_VERSION,
        TYPED_JSON_FORMAT_ABI_MISMATCH, TYPED_JSON_FORMAT_DEPTH_LIMIT,
        TYPED_JSON_FORMAT_DESCRIPTOR_INVALID, TYPED_JSON_FORMAT_INVALID_ARGUMENT,
        TYPED_JSON_FORMAT_NON_FINITE_NUMBER, TYPED_JSON_FORMAT_OK,
        TYPED_JSON_FORMAT_RESOURCE_LIMIT,
    };

    use super::{
        FormatFailure, JsonStage, json_format_typed_v1, json_format_typed_with_object_limit,
    };
    use crate::gc::{activate_runtime_v1, deactivate_runtime_v1};
    use crate::runtime::{LoomRuntime, runtime_create_v1, runtime_destroy_v1};
    use crate::text::{
        TextObject, allocate_byte_storage, allocate_text_storage, allocate_typed_text,
        scalar_length, text_bytes,
    };

    const DESCRIPTOR_BYTES: usize = size_of::<LoomTypedJsonLayout>() + 1;

    #[repr(align(8))]
    struct AlignedDescriptorBytes([u8; DESCRIPTOR_BYTES]);

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct DirectJson {
        tag: u8,
        padding: [u8; 7],
        scalar: u64,
        pointer: *const c_void,
    }

    impl DirectJson {
        const fn null() -> Self {
            Self {
                tag: 0,
                padding: [0; 7],
                scalar: 0,
                pointer: ptr::null(),
            }
        }

        const fn boolean(value: bool) -> Self {
            Self {
                tag: 1,
                padding: [0; 7],
                scalar: value as u64,
                pointer: ptr::null(),
            }
        }

        fn number(value: f64) -> Self {
            Self {
                tag: 2,
                padding: [0; 7],
                scalar: value.to_bits(),
                pointer: ptr::null(),
            }
        }

        const fn text(pointer: *const c_void) -> Self {
            Self {
                tag: 3,
                padding: [0; 7],
                scalar: 0,
                pointer,
            }
        }

        const fn array(pointer: *const c_void) -> Self {
            Self {
                tag: 4,
                padding: [0; 7],
                scalar: 0,
                pointer,
            }
        }

        const fn object(pointer: *const c_void) -> Self {
            Self {
                tag: 5,
                padding: [0; 7],
                scalar: 0,
                pointer,
            }
        }
    }

    #[repr(C)]
    struct DirectList<const N: usize> {
        length: u64,
        capacity: u64,
        data: [DirectJson; N],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct DirectMapEntry {
        key: *const c_void,
        value: DirectJson,
    }

    #[repr(C)]
    struct DirectMap<const N: usize> {
        length: u64,
        data: [DirectMapEntry; N],
    }

    fn layout() -> LoomTypedJsonLayout {
        LoomTypedJsonLayout {
            abi_version: TYPED_JSON_ABI_VERSION,
            flags: 0,
            json_size: size_of::<DirectJson>() as u64,
            json_alignment: align_of::<DirectJson>() as u64,
            tag_offset: offset_of!(DirectJson, tag) as u64,
            tag_size: size_of::<u8>() as u64,
            bool_payload_offset: offset_of!(DirectJson, scalar) as u64,
            number_payload_offset: offset_of!(DirectJson, scalar) as u64,
            text_payload_offset: offset_of!(DirectJson, pointer) as u64,
            array_payload_offset: offset_of!(DirectJson, pointer) as u64,
            object_payload_offset: offset_of!(DirectJson, pointer) as u64,
            list_length_offset: offset_of!(DirectList<1>, length) as u64,
            list_capacity_offset: offset_of!(DirectList<1>, capacity) as u64,
            list_data_offset: offset_of!(DirectList<1>, data) as u64,
            list_element_stride: size_of::<DirectJson>() as u64,
            map_length_offset: offset_of!(DirectMap<1>, length) as u64,
            map_data_offset: offset_of!(DirectMap<1>, data) as u64,
            map_entry_stride: size_of::<DirectMapEntry>() as u64,
            map_key_offset: offset_of!(DirectMapEntry, key) as u64,
            map_value_offset: offset_of!(DirectMapEntry, value) as u64,
        }
    }

    struct ActiveRuntime(*mut LoomRuntime);

    impl ActiveRuntime {
        fn new() -> Self {
            let runtime = runtime_create_v1();
            assert!(!runtime.is_null());
            assert_eq!(unsafe { activate_runtime_v1(runtime) }, GC_OK);
            Self(runtime)
        }
    }

    impl Drop for ActiveRuntime {
        fn drop(&mut self) {
            assert_eq!(unsafe { deactivate_runtime_v1(self.0) }, GC_OK);
            assert_eq!(unsafe { runtime_destroy_v1(self.0) }, GC_OK);
        }
    }

    fn formatted(json: &DirectJson, descriptor: &LoomTypedJsonLayout) -> String {
        let _runtime = ActiveRuntime::new();
        let mut output = ptr::null_mut();
        assert_eq!(
            unsafe {
                json_format_typed_v1(
                    (ptr::from_ref(json)).cast(),
                    ptr::from_ref(descriptor),
                    &raw mut output,
                )
            },
            TYPED_JSON_FORMAT_OK,
        );
        let bytes = unsafe { text_bytes(output) }.expect("typed JSON Text output");
        let value = std::str::from_utf8(bytes).expect("canonical JSON UTF-8");
        assert_eq!(
            unsafe { scalar_length(output.cast::<TextObject>()) },
            Some(value.chars().count() as u64),
        );
        value.to_owned()
    }

    fn status(json: &DirectJson, descriptor: &LoomTypedJsonLayout) -> i32 {
        let sentinel = ptr::dangling_mut::<u64>().cast::<c_void>();
        let mut output = sentinel;
        let status = unsafe {
            json_format_typed_v1(
                (ptr::from_ref(json)).cast(),
                ptr::from_ref(descriptor),
                &raw mut output,
            )
        };
        assert_eq!(output, sentinel, "failure must not publish output");
        status
    }

    #[test]
    fn direct_variants_preserve_canonical_format_order_escaping_and_negative_zero() {
        let descriptor = layout();
        assert_eq!(formatted(&DirectJson::null(), &descriptor), "null");
        assert_eq!(formatted(&DirectJson::boolean(false), &descriptor), "false");
        assert_eq!(formatted(&DirectJson::boolean(true), &descriptor), "true");
        assert_eq!(formatted(&DirectJson::number(-0.0), &descriptor), "-0");

        let (text_storage, text) = allocate_text_storage("line\n\"界\u{1}".as_bytes()).unwrap();
        assert_eq!(
            formatted(&DirectJson::text(text.cast()), &descriptor),
            "\"line\\n\\\"界\\u0001\"",
        );

        let array = DirectList {
            length: 4,
            capacity: 4,
            data: [
                DirectJson::null(),
                DirectJson::boolean(true),
                DirectJson::number(12.5),
                DirectJson::text(text.cast()),
            ],
        };
        assert_eq!(
            formatted(
                &DirectJson::array((ptr::from_ref(&array)).cast()),
                &descriptor,
            ),
            "[null,true,12.5,\"line\\n\\\"界\\u0001\"]",
        );

        let (a_storage, a) = allocate_text_storage(b"a").unwrap();
        let (z_storage, z) = allocate_text_storage(b"z").unwrap();
        let object = DirectMap {
            length: 2,
            data: [
                DirectMapEntry {
                    key: a.cast(),
                    value: DirectJson::boolean(false),
                },
                DirectMapEntry {
                    key: z.cast(),
                    value: DirectJson::number(-0.0),
                },
            ],
        };
        assert_eq!(
            formatted(
                &DirectJson::object((ptr::from_ref(&object)).cast()),
                &descriptor,
            ),
            "{\"a\":false,\"z\":-0}",
        );
        assert_eq!(
            formatted(&DirectJson::array(ptr::null()), &descriptor),
            "[]"
        );
        assert_eq!(
            formatted(&DirectJson::object(ptr::null()), &descriptor),
            "{}"
        );
        drop((z_storage, a_storage, text_storage));
    }

    #[test]
    fn descriptor_and_boundary_validation_are_strict_and_transactional() {
        let json = DirectJson::null();
        let descriptor = layout();
        let sentinel = ptr::dangling_mut::<u64>().cast::<c_void>();
        let mut output = sentinel;
        assert_eq!(
            unsafe {
                json_format_typed_v1(ptr::null(), ptr::from_ref(&descriptor), &raw mut output)
            },
            TYPED_JSON_FORMAT_INVALID_ARGUMENT,
        );
        assert_eq!(output, sentinel);
        assert_eq!(
            unsafe {
                json_format_typed_v1((ptr::from_ref(&json)).cast(), ptr::null(), &raw mut output)
            },
            TYPED_JSON_FORMAT_INVALID_ARGUMENT,
        );
        assert_eq!(output, sentinel);
        assert_eq!(
            unsafe {
                json_format_typed_v1(
                    (ptr::from_ref(&json)).cast(),
                    ptr::from_ref(&descriptor),
                    ptr::null_mut(),
                )
            },
            TYPED_JSON_FORMAT_INVALID_ARGUMENT,
        );

        let mut mismatch = descriptor;
        mismatch.abi_version += 1;
        assert_eq!(status(&json, &mismatch), TYPED_JSON_FORMAT_ABI_MISMATCH);
        let mut too_large = descriptor;
        too_large.json_size = GC_MAX_OBJECT_BYTES + 1;
        assert_eq!(status(&json, &too_large), TYPED_JSON_FORMAT_RESOURCE_LIMIT);

        let mut malformed = Vec::new();
        macro_rules! bad {
            ($field:ident, $value:expr) => {{
                let mut value = descriptor;
                value.$field = $value;
                malformed.push((stringify!($field), value));
            }};
        }
        bad!(flags, 1);
        bad!(json_size, 0);
        bad!(json_alignment, 3);
        bad!(tag_offset, descriptor.json_size);
        bad!(tag_size, 3);
        bad!(bool_payload_offset, descriptor.tag_offset);
        bad!(number_payload_offset, 9);
        bad!(text_payload_offset, descriptor.json_size);
        bad!(array_payload_offset, descriptor.json_size);
        bad!(object_payload_offset, descriptor.json_size);
        bad!(list_length_offset, 1);
        bad!(list_capacity_offset, descriptor.list_length_offset);
        bad!(list_data_offset, 8);
        bad!(list_data_offset, GC_MAX_OBJECT_BYTES);
        bad!(list_element_stride, 8);
        bad!(map_length_offset, 1);
        bad!(map_data_offset, 4);
        bad!(map_data_offset, GC_MAX_OBJECT_BYTES);
        bad!(map_entry_stride, 16);
        bad!(map_key_offset, 1);
        bad!(map_value_offset, 4);
        for (field, malformed) in malformed {
            assert_eq!(
                status(&json, &malformed),
                TYPED_JSON_FORMAT_DESCRIPTOR_INVALID,
                "field {field}",
            );
        }

        let mut unaligned_descriptor = AlignedDescriptorBytes([0; DESCRIPTOR_BYTES]);
        let descriptor_pointer = unsafe { unaligned_descriptor.0.as_mut_ptr().add(1) };
        assert!(
            !descriptor_pointer
                .addr()
                .is_multiple_of(align_of::<LoomTypedJsonLayout>())
        );
        assert_eq!(
            unsafe {
                json_format_typed_v1(
                    (ptr::from_ref(&json)).cast(),
                    descriptor_pointer.cast(),
                    &raw mut output,
                )
            },
            TYPED_JSON_FORMAT_INVALID_ARGUMENT,
        );
        assert_eq!(output, sentinel);
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one table-driven test covers malformed values and both direct containers"
    )]
    fn malformed_direct_values_and_noncanonical_containers_are_rejected() {
        let descriptor = layout();
        let mut invalid_tag = DirectJson::null();
        invalid_tag.tag = 6;
        assert_eq!(
            status(&invalid_tag, &descriptor),
            TYPED_JSON_FORMAT_INVALID_ARGUMENT
        );
        let mut invalid_bool = DirectJson::boolean(false);
        invalid_bool.scalar = 2;
        assert_eq!(
            status(&invalid_bool, &descriptor),
            TYPED_JSON_FORMAT_INVALID_ARGUMENT
        );
        assert_eq!(
            status(&DirectJson::text(ptr::null()), &descriptor),
            TYPED_JSON_FORMAT_INVALID_ARGUMENT
        );

        let (_bytes_storage, bytes) = allocate_byte_storage(b"not Text").unwrap();
        assert_eq!(
            status(&DirectJson::text(bytes.cast()), &descriptor),
            TYPED_JSON_FORMAT_INVALID_ARGUMENT
        );
        let (_text_storage, text) = allocate_text_storage("界".as_bytes()).unwrap();
        unsafe { (*text).scalar_length = 2 };
        assert_eq!(
            status(&DirectJson::text(text.cast()), &descriptor),
            TYPED_JSON_FORMAT_INVALID_ARGUMENT
        );

        let bad_list = DirectList {
            length: 2,
            capacity: 1,
            data: [DirectJson::null()],
        };
        assert_eq!(
            status(
                &DirectJson::array((ptr::from_ref(&bad_list)).cast()),
                &descriptor,
            ),
            TYPED_JSON_FORMAT_INVALID_ARGUMENT
        );
        let empty_list = DirectList::<0> {
            length: 0,
            capacity: 0,
            data: [],
        };
        assert_eq!(
            status(
                &DirectJson::array((ptr::from_ref(&empty_list)).cast()),
                &descriptor,
            ),
            TYPED_JSON_FORMAT_INVALID_ARGUMENT
        );

        let (a_storage, a) = allocate_text_storage(b"a").unwrap();
        let (z_storage, z) = allocate_text_storage(b"z").unwrap();
        let unsorted = DirectMap {
            length: 2,
            data: [
                DirectMapEntry {
                    key: z.cast(),
                    value: DirectJson::null(),
                },
                DirectMapEntry {
                    key: a.cast(),
                    value: DirectJson::null(),
                },
            ],
        };
        assert_eq!(
            status(
                &DirectJson::object((ptr::from_ref(&unsorted)).cast()),
                &descriptor,
            ),
            TYPED_JSON_FORMAT_INVALID_ARGUMENT
        );
        let duplicate = DirectMap {
            length: 2,
            data: [
                DirectMapEntry {
                    key: a.cast(),
                    value: DirectJson::null(),
                },
                DirectMapEntry {
                    key: a.cast(),
                    value: DirectJson::null(),
                },
            ],
        };
        assert_eq!(
            status(
                &DirectJson::object((ptr::from_ref(&duplicate)).cast()),
                &descriptor,
            ),
            TYPED_JSON_FORMAT_INVALID_ARGUMENT
        );
        let empty_map = DirectMap::<0> {
            length: 0,
            data: [],
        };
        assert_eq!(
            status(
                &DirectJson::object((ptr::from_ref(&empty_map)).cast()),
                &descriptor,
            ),
            TYPED_JSON_FORMAT_INVALID_ARGUMENT
        );
        drop((z_storage, a_storage));
    }

    #[test]
    fn depth_and_nonfinite_failures_use_only_the_public_result_statuses() {
        let descriptor = layout();
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                status(&DirectJson::number(value), &descriptor),
                TYPED_JSON_FORMAT_NON_FINITE_NUMBER
            );
        }

        let mut root = DirectJson::null();
        let mut containers = Vec::new();
        for _ in 0..128 {
            containers.push(Box::new(DirectList {
                length: 1,
                capacity: 1,
                data: [root],
            }));
            root = DirectJson::array(
                (ptr::from_ref(containers.last().expect("nested list").as_ref())).cast(),
            );
        }
        let expected = format!("{}null{}", "[".repeat(128), "]".repeat(128));
        assert_eq!(formatted(&root, &descriptor), expected);

        containers.push(Box::new(DirectList {
            length: 1,
            capacity: 1,
            data: [root],
        }));
        root = DirectJson::array(
            (ptr::from_ref(containers.last().expect("beyond-limit list").as_ref())).cast(),
        );
        assert_eq!(status(&root, &descriptor), TYPED_JSON_FORMAT_DEPTH_LIMIT);
    }

    #[test]
    fn escaped_output_over_managed_object_budget_is_resource_limited_and_transactional() {
        let descriptor = layout();
        let (_text_storage, text) = allocate_text_storage(b"\x01").unwrap();
        let json = DirectJson::text(text.cast());
        let production_stage = JsonStage::for_object_limit(GC_MAX_OBJECT_BYTES).unwrap();
        let production_maximum = usize::try_from(
            GC_MAX_OBJECT_BYTES
                .checked_sub(loom_runtime_abi::TEXT_OBJECT_HEADER_SIZE)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            production_stage.checked_output_length(production_maximum + 1),
            Err(FormatFailure::ResourceLimit),
        );

        // `"\u0001"` needs eight output bytes. Exercise the same object-limit
        // calculation used with GC_MAX_OBJECT_BYTES in production at a scaled
        // limit, rather than making every test run allocate nearly one GiB.
        let object_limit = loom_runtime_abi::TEXT_OBJECT_HEADER_SIZE + 7;
        let sentinel = ptr::dangling_mut::<u64>().cast::<c_void>();
        let mut output = sentinel;
        assert_eq!(
            unsafe {
                json_format_typed_with_object_limit(
                    (ptr::from_ref(&json)).cast(),
                    ptr::from_ref(&descriptor),
                    &raw mut output,
                    object_limit,
                )
            },
            TYPED_JSON_FORMAT_RESOURCE_LIMIT,
        );
        assert_eq!(output, sentinel, "resource failure must not publish Text");
    }

    #[test]
    fn complete_staging_precedes_the_only_managed_text_allocation() {
        let runtime = ActiveRuntime::new();
        let mut source = ptr::null_mut();
        assert_eq!(
            unsafe { allocate_typed_text("moving 界🙂".as_bytes(), 9, &raw mut source) },
            GC_OK,
        );
        unsafe { (*runtime.0).heap.collect_before_every_allocation = true };

        let json = DirectJson::text(source);
        let descriptor = layout();
        let mut output = ptr::null_mut();
        assert_eq!(
            unsafe {
                json_format_typed_v1(
                    (ptr::from_ref(&json)).cast(),
                    ptr::from_ref(&descriptor),
                    &raw mut output,
                )
            },
            TYPED_JSON_FORMAT_OK,
        );
        assert_eq!(
            unsafe { text_bytes(output) },
            Some(&b"\"moving \xe7\x95\x8c\xf0\x9f\x99\x82\""[..]),
        );
        assert!(unsafe { (*runtime.0).heap.collections } >= 1);
        unsafe { (*runtime.0).heap.collect_before_every_allocation = false };
    }
}
