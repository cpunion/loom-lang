//! Private Rust boundary for separately linked native providers. Not a Loom API.
//! Native contexts own no managed pointers and live for one Task owner.

use crate::{List, reserve};

// Private worker activation protocol, not a source spawn API. The caller must
// separately attach a mutator and keep its managed handoff rooted until drain.
pub use crate::worker_control::{Control as WorkerControl, Exit as WorkerExit};

// One-shot owner notification for a native provider. No managed references,
// callbacks, or OS handles cross this boundary. Cancelled registrations are
// harmless when a provider later notifies them.
pub struct Completion {
    pub(crate) reactor: std::sync::Arc<crate::wait::Reactor>,
    pub(crate) registration: crate::wait::Registration,
}

impl Completion {
    pub fn notify(self) {
        crate::tasks::notify_completion(self);
    }
}

// `ready` is evaluated by the owner before registration, e.g. by comparing a
// provider generation. This closes the notify-before-registration race.
pub fn wait_completion(identity: u64, ready: bool) -> Option<Completion> {
    crate::tasks::provider_completion(identity, ready)
}

pub fn context<T: Default + 'static, R>(operation: impl FnOnce(&T) -> R) -> R {
    crate::tasks::native_context(operation)
}

/// # Safety
/// `pointer` is a live Text; the result cannot survive a managed allocation.
pub unsafe fn text<'a>(pointer: *const u8) -> &'a [u8] {
    unsafe { crate::text_bytes(pointer) }
}

/// # Safety
/// `pointer` is a live Bytes; the result cannot survive a managed allocation.
pub unsafe fn bytes<'a>(pointer: *const u8) -> &'a [u8] {
    unsafe { crate::buffer_bytes(pointer) }
}

/// # Safety
/// `pointer` is a live List[Text]. Copies retain no managed references.
pub unsafe fn texts(pointer: *const u8) -> Vec<Vec<u8>> {
    let list = unsafe { &*pointer.cast::<List>() };
    (0..list.buffer.len)
        .map(|index| unsafe {
            text(
                *list
                    .buffer
                    .data
                    .add(index * list.stride)
                    .cast::<*const u8>(),
            )
            .to_vec()
        })
        .collect()
}

/// # Safety
/// `pointer` is a rooted Bytes. `bytes` must not reference managed storage:
/// reserving output capacity can move the managed heap.
pub unsafe fn append(pointer: *mut u8, bytes: &[u8]) -> i64 {
    if !bytes.is_empty() {
        unsafe {
            let buffer = reserve(pointer, bytes.len(), 1);
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                (*buffer).data.add((*buffer).len),
                bytes.len(),
            );
            (*buffer).len += bytes.len();
        }
    }
    bytes.len() as i64
}

/// # Safety
/// Input must be valid UTF-8 in independent native storage, not managed storage.
pub unsafe fn copy_text(bytes: &[u8]) -> *mut u8 {
    unsafe { crate::loom_rt_text_new(bytes.as_ptr(), bytes.len()) }
}
