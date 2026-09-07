//! Loom's single-threaded, moving managed-memory and platform boundary.
//! Generated code roots every live managed slot before an allocating call.

use std::alloc::{Layout, alloc, alloc_zeroed, dealloc, realloc};
#[cfg(not(windows))]
use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashMap;
#[cfg(not(windows))]
use std::ffi::CStr;
use std::ffi::c_char;
use std::hash::{BuildHasherDefault, Hasher};
use std::ptr::{self, NonNull};

mod file_io;
mod process_io;

type Trace = unsafe extern "C" fn(*mut u8);

#[repr(C)]
#[derive(Clone, Copy)]
struct Root {
    address: *mut u8,
    trace: Trace,
}

#[repr(C)]
struct RootFrame {
    previous: *mut RootFrame,
    roots: *const Root,
    count: usize,
}

unsafe extern "C" fn trace_pointer(address: *mut u8) {
    // SAFETY: Pointer roots and managed list elements are initialized slots.
    unsafe { *address.cast::<*mut u8>() = loom_rt_visit(*address.cast::<*mut u8>()) };
}

struct Allocation {
    size: usize,
    trace: Option<Trace>,
    forwarded: *mut u8,
}

impl Allocation {
    fn layout(&self) -> Layout {
        // SAFETY: Entries are created only from validated 16-aligned layouts.
        unsafe { Layout::from_size_align_unchecked(self.size, 16) }
    }

    fn individually_owned(&self) -> bool {
        // Every small allocation, including initial storage and GC copies,
        // belongs to an arena. Large allocations always own separate storage.
        self.size >= LARGE_OBJECT_THRESHOLD
    }

    fn growth_cost(&self, layout: Layout) -> usize {
        if self.individually_owned() {
            layout.size() - self.size
        } else {
            occupied(layout) // The previous arena slice stays occupied until GC.
        }
    }
}

fn occupied(layout: Layout) -> usize {
    if layout.size() < LARGE_OBJECT_THRESHOLD {
        layout.pad_to_align().size()
    } else {
        layout.size()
    }
}

// Keys are allocator-generated addresses, never source-controlled strings.
// Multiplication spreads aligned pointer bits through both table index and tag.
#[derive(Default)]
struct AddressHasher(u64);

impl Hasher for AddressHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 = (self.0 ^ u64::from(*byte)).wrapping_mul(0x9e3779b97f4a7c15);
        }
    }

    fn write_usize(&mut self, address: usize) {
        let mixed = ((address >> 4) as u64).wrapping_mul(0x9e3779b97f4a7c15);
        self.0 = mixed ^ (mixed >> 32);
    }
}

type Objects = HashMap<usize, Allocation, BuildHasherDefault<AddressHasher>>;

#[derive(Default)]
struct Arena {
    pages: Vec<NonNull<u8>>,
    used: usize,
}

impl Arena {
    fn allocate(&mut self, layout: Layout) -> NonNull<u8> {
        let size = layout.pad_to_align().size();
        if self.pages.is_empty() || self.used + size > PAGE_SIZE {
            let layout = Layout::from_size_align(PAGE_SIZE, 16).unwrap();
            // SAFETY: Small objects fit within a page and are aligned to 16 bytes.
            let page =
                NonNull::new(unsafe { alloc(layout) }).unwrap_or_else(|| fault("out of memory"));
            self.pages.push(page);
            self.used = 0;
        }
        // SAFETY: The page owns this aligned, disjoint range until arena clear.
        let pointer =
            unsafe { NonNull::new_unchecked(self.pages.last().unwrap().as_ptr().add(self.used)) };
        self.used += size;
        pointer
    }

    fn clear(&mut self) {
        let layout = Layout::from_size_align(PAGE_SIZE, 16).unwrap();
        for page in self.pages.drain(..) {
            // SAFETY: All page slots are dead before the owning arena is cleared.
            unsafe { dealloc(page.as_ptr(), layout) };
        }
        self.used = 0;
    }
}

impl Drop for Arena {
    fn drop(&mut self) {
        self.clear();
    }
}

struct Heap {
    objects: Objects,
    previous: Objects,
    space: Arena,
    previous_space: Arena,
    collecting: bool,
    roots: *mut RootFrame,
    work: Vec<(*mut u8, Trace)>,
    bytes: usize, // Occupied slots, including abandoned growth slices until GC.
    threshold: usize,
    stress: bool,
}

const MIN_THRESHOLD: usize = 64 * 1024;
const LARGE_OBJECT_THRESHOLD: usize = 64 * 1024;
const PAGE_SIZE: usize = 256 * 1024;

impl Default for Heap {
    fn default() -> Self {
        Self {
            objects: Objects::default(),
            previous: Objects::default(),
            space: Arena::default(),
            previous_space: Arena::default(),
            collecting: false,
            roots: ptr::null_mut(),
            work: Vec::new(),
            bytes: 0,
            threshold: MIN_THRESHOLD,
            stress: std::env::var_os("LOOM_GC_STRESS").as_deref()
                == Some(std::ffi::OsStr::new("1")),
        }
    }
}

impl Drop for Heap {
    fn drop(&mut self) {
        for (address, allocation) in self.objects.iter().chain(
            self.previous
                .iter()
                .filter(|(address, object)| object.forwarded as usize != **address),
        ) {
            if allocation.individually_owned() {
                // SAFETY: Large entries own their allocation, except old
                // self-forwarded entries above. Small slots belong to arenas.
                unsafe { dealloc(*address as *mut u8, allocation.layout()) };
            }
        }
    }
}

thread_local! {
    static HEAP: RefCell<Heap> = RefCell::new(Heap::default());
    #[cfg(not(windows))]
    static PROCESS_ARGS: Cell<(i32, *const *const c_char)> = const { Cell::new((0, ptr::null())) };
    #[cfg(windows)]
    static PROCESS_ARGS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn fault(message: &str) -> ! {
    eprintln!("RuntimeFault: {message}");
    std::process::abort()
}

fn allocate(size: usize, trace: Option<Trace>) -> *mut u8 {
    allocate_storage(size, trace, true)
}

fn storage(arena: &mut Arena, layout: Layout, zeroed: bool) -> NonNull<u8> {
    if layout.size() < LARGE_OBJECT_THRESHOLD {
        let pointer = arena.allocate(layout);
        if zeroed {
            // SAFETY: This fresh slot has layout.size() writable bytes.
            unsafe { ptr::write_bytes(pointer.as_ptr(), 0, layout.size()) };
        }
        pointer
    } else {
        // SAFETY: Large storage is separately owned and uses this same layout
        // for deallocation or reallocation. Raw capacity need not be zeroed.
        NonNull::new(unsafe {
            if zeroed {
                alloc_zeroed(layout)
            } else {
                alloc(layout)
            }
        })
        .unwrap_or_else(|| fault("out of memory"))
    }
}

fn allocate_storage(size: usize, trace: Option<Trace>, zeroed: bool) -> *mut u8 {
    let layout = Layout::from_size_align(size.max(1), 16)
        .unwrap_or_else(|_| fault("allocation size overflow"));
    let collect = HEAP.with(|heap| {
        let heap = heap.borrow();
        if heap.collecting {
            fault("allocation during GC tracing");
        }
        heap.stress || heap.bytes.saturating_add(occupied(layout)) >= heap.threshold
    });
    if collect {
        loom_rt_collect();
    }
    HEAP.with(|heap| {
        let mut heap = heap.borrow_mut();
        let pointer = storage(&mut heap.space, layout, zeroed);
        heap.bytes += occupied(layout);
        heap.objects.insert(
            pointer.as_ptr() as usize,
            Allocation {
                size: layout.size(),
                trace,
                forwarded: ptr::null_mut(),
            },
        );
        pointer.as_ptr()
    })
}

// Generated code roots the input, then initializes this zeroed payload before
// the next allocation. The supplied tracer describes the stored concrete value.
#[unsafe(no_mangle)]
extern "C" fn loom_rt_box_new(size: usize, trace: Option<Trace>) -> *mut u8 {
    allocate(size, trace)
}

// Root entry/exit never trigger Loom GC; generated return snapshots remain
// valid while their frame is detached and the caller receives the value.
#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_roots_enter(frame: *mut RootFrame, roots: *const Root, count: usize) {
    HEAP.with(|heap| {
        let mut heap = heap.borrow_mut();
        // SAFETY: The caller provides writable frame storage and count live
        // root entries. Their addresses stay fixed until the matching leave;
        // initialize every field before publishing this single-threaded head.
        unsafe {
            ptr::write(
                frame,
                RootFrame {
                    previous: heap.roots,
                    roots,
                    count,
                },
            );
        }
        heap.roots = frame;
    });
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_roots_leave(frame: *mut RootFrame) {
    HEAP.with(|heap| {
        let mut heap = heap.borrow_mut();
        if heap.roots != frame {
            fault("invalid GC root frame order");
        }
        // SAFETY: A matching enter initialized this still-live frame.
        heap.roots = unsafe { (*frame).previous };
    });
}

#[unsafe(no_mangle)]
extern "C" fn loom_rt_visit(pointer: *mut u8) -> *mut u8 {
    HEAP.with(|heap| {
        let mut heap = heap.borrow_mut();
        let retain_large = !heap.stress;
        let Heap {
            objects,
            previous,
            space,
            bytes,
            work,
            ..
        } = &mut *heap;
        let Some(object) = previous.get_mut(&(pointer as usize)) else {
            return pointer; // Null, static Text, or an already relocated pointer.
        };
        if !object.forwarded.is_null() {
            return object.forwarded;
        }
        let layout = object.layout();
        let trace = object.trace;
        let moved = if retain_large && layout.size() >= LARGE_OBJECT_THRESHOLD {
            // Avoid copying large backing buffers immediately before growth.
            // This is an internal policy, not a stable-address guarantee.
            // SAFETY: Present keys identify nonnull allocated storage.
            unsafe { NonNull::new_unchecked(pointer) }
        } else {
            // SAFETY: Keep from-space alive until all root/field rewriting ends.
            // Bytewise copy preserves raw capacity; stress moves every size.
            let moved = storage(space, layout, false);
            unsafe { ptr::copy_nonoverlapping(pointer, moved.as_ptr(), layout.size()) };
            moved
        };
        object.forwarded = moved.as_ptr();
        *bytes += occupied(layout);
        objects.insert(
            moved.as_ptr() as usize,
            Allocation {
                size: layout.size(),
                trace,
                forwarded: ptr::null_mut(),
            },
        );
        if let Some(trace) = trace {
            work.push((moved.as_ptr(), trace));
        }
        moved.as_ptr()
    })
}

#[unsafe(no_mangle)]
extern "C" fn loom_rt_collect() {
    let mut frame = HEAP.with(|heap| {
        let mut heap = heap.borrow_mut();
        if heap.collecting {
            fault("recursive GC collection");
        }
        heap.collecting = true;
        heap.work.clear();
        // The empty from-space table retains its buckets for the next live set.
        let Heap {
            objects,
            previous,
            space,
            previous_space,
            ..
        } = &mut *heap;
        std::mem::swap(objects, previous);
        std::mem::swap(space, previous_space);
        heap.bytes = 0;
        heap.roots
    });
    while !frame.is_null() {
        // SAFETY: Active stack frames and slots stay fixed. Tracers rewrite
        // managed pointers; they cannot allocate,
        // collect or modify the root chain. No Heap borrow spans a callback.
        unsafe {
            let active = &*frame;
            for index in 0..active.count {
                let root = *active.roots.add(index);
                (root.trace)(root.address);
            }
            frame = active.previous;
        }
    }
    loop {
        let item = HEAP.with(|heap| heap.borrow_mut().work.pop());
        let Some((pointer, trace)) = item else { break };
        // SAFETY: The relocated allocation is live. No heap borrow spans callbacks,
        // which may visit more objects but must not allocate or collect.
        unsafe { trace(pointer) };
    }
    HEAP.with(|heap| {
        let mut heap = heap.borrow_mut();
        for (address, object) in &heap.previous {
            if object.individually_owned() && object.forwarded as usize != *address {
                // SAFETY: Roots/fields now address to-space; self-forwarded
                // allocations belong to objects and must not be freed here.
                unsafe { dealloc(*address as *mut u8, object.layout()) };
            }
        }
        heap.previous.clear();
        heap.previous_space.clear();
        heap.threshold = heap.bytes.saturating_mul(2).max(MIN_THRESHOLD);
        heap.collecting = false;
    });
}

// Runtime calls own their argument snapshots, just like generated code. Only
// raw slot pointers cross collection: no Rust reference aliases a rewritten slot.
fn rooted<const N: usize, R>(
    mut values: [*mut u8; N],
    operation: impl FnOnce(*mut *mut u8) -> R,
) -> R {
    let slots = ptr::addr_of_mut!(values).cast::<*mut u8>();
    let roots: [Root; N] = std::array::from_fn(|index| Root {
        // SAFETY: index is within the fixed, address-taken stack array.
        address: unsafe { slots.add(index) }.cast(),
        trace: trace_pointer,
    });
    let mut frame = std::mem::MaybeUninit::<RootFrame>::uninit();
    // SAFETY: Slots, entries and frame remain live until the operation returns.
    unsafe { loom_rt_roots_enter(frame.as_mut_ptr(), roots.as_ptr(), N) };
    let output = operation(slots);
    unsafe { loom_rt_roots_leave(frame.as_mut_ptr()) };
    output
}

#[repr(C)]
struct Text {
    len: usize,
    data: [u8; 0],
}

unsafe fn text_bytes<'a>(text: *const u8) -> &'a [u8] {
    // SAFETY: Callers pass a live Text header followed by exactly len bytes.
    unsafe {
        let text = text.cast::<Text>();
        std::slice::from_raw_parts((*text).data.as_ptr(), (*text).len)
    }
}

fn allocate_text(len: usize) -> *mut Text {
    let size = size_of::<Text>()
        .checked_add(len)
        .unwrap_or_else(|| fault("text size overflow"));
    let text = allocate(size, None).cast::<Text>();
    // SAFETY: The allocation contains the header and exactly len payload bytes.
    unsafe { (*text).len = len };
    text
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_text_new(bytes: *const u8, len: usize) -> *mut u8 {
    // SAFETY: This entry accepts only independently owned or static bytes,
    // never a managed interior pointer. A zero-length buffer may be null.
    let bytes = if len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(bytes, len) }
    };
    if std::str::from_utf8(bytes).is_err() {
        fault("invalid UTF-8 text");
    }
    let result = allocate_text(len);
    // SAFETY: result has space for the header and bytes; its storage is disjoint.
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), (*result).data.as_mut_ptr(), len);
    }
    result.cast()
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_float_parse(text: *const u8) -> f64 {
    // SAFETY: The caller supplies live UTF-8 Text; parsing never collects.
    let text = unsafe { std::str::from_utf8_unchecked(text_bytes(text)) };
    text.parse().unwrap_or_else(|_| fault("invalid Float text"))
}

#[unsafe(no_mangle)]
extern "C" fn loom_rt_float_format(value: f64) -> *mut u8 {
    let text = value.to_string();
    // SAFETY: Rust owns these UTF-8 bytes across the managed allocation/copy.
    unsafe { loom_rt_text_new(text.as_ptr(), text.len()) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_text_concat(left: *const u8, right: *const u8) -> *mut u8 {
    rooted([left.cast_mut(), right.cast_mut()], |slots| {
        // SAFETY: Read lengths before allocation; obtain fresh interior pointers
        // from the rewritten input slots only after allocation has finished.
        unsafe {
            let len = (*(*slots).cast::<Text>())
                .len
                .checked_add((*(*slots.add(1)).cast::<Text>()).len)
                .unwrap_or_else(|| fault("text size overflow"));
            let result = allocate_text(len);
            let (left, right) = (text_bytes(*slots), text_bytes(*slots.add(1)));
            let data = (*result).data.as_mut_ptr();
            ptr::copy_nonoverlapping(left.as_ptr(), data, left.len());
            ptr::copy_nonoverlapping(right.as_ptr(), data.add(left.len()), right.len());
            result.cast()
        }
    })
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_text_equal(left: *const u8, right: *const u8) -> i32 {
    // SAFETY: Both pointers denote live Text objects or static literals.
    i32::from(unsafe { text_bytes(left) == text_bytes(right) })
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_text_slice(text: *const u8, start: i64, end: i64) -> *mut u8 {
    let (start, end) = usize::try_from(start)
        .ok()
        .zip(usize::try_from(end).ok())
        .unwrap_or_else(|| fault("invalid text slice range or UTF-8 boundary"));
    rooted([text.cast_mut()], |slots| {
        // SAFETY: Validation borrows the input only before allocation. The copy
        // reloads its rooted header and reconstructs the same byte offset.
        unsafe {
            if std::str::from_utf8_unchecked(text_bytes(*slots))
                .get(start..end)
                .is_none()
            {
                fault("invalid text slice range or UTF-8 boundary");
            }
            let result = allocate_text(end - start);
            ptr::copy_nonoverlapping(
                text_bytes(*slots).as_ptr().add(start),
                (*result).data.as_mut_ptr(),
                end - start,
            );
            result.cast()
        }
    })
}

fn unicode_scalar(value: i64) -> Option<char> {
    u32::try_from(value).ok().and_then(char::from_u32)
}

#[unsafe(no_mangle)]
extern "C" fn loom_rt_unicode_alphabetic(value: i64) -> i32 {
    i32::from(unicode_scalar(value).is_some_and(char::is_alphabetic))
}

#[unsafe(no_mangle)]
extern "C" fn loom_rt_unicode_alphanumeric(value: i64) -> i32 {
    i32::from(unicode_scalar(value).is_some_and(char::is_alphanumeric))
}

#[unsafe(no_mangle)]
extern "C" fn loom_rt_unicode_whitespace(value: i64) -> i32 {
    i32::from(unicode_scalar(value).is_some_and(char::is_whitespace))
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_process_init(argc: i32, argv: *const *const c_char) {
    if argc < 0 || (argc != 0 && argv.is_null()) {
        fault("invalid process arguments");
    }
    // Unix argv preserves the original bytes. Windows C argv can use an ANSI
    // code page; Rust reads the native wide command line instead.
    #[cfg(not(windows))]
    PROCESS_ARGS.set((argc, argv));
    #[cfg(windows)]
    PROCESS_ARGS.with(|arguments| {
        *arguments.borrow_mut() = std::env::args_os()
            .map(|argument| {
                argument
                    .into_string()
                    .unwrap_or_else(|_| fault("invalid Unicode process argument"))
            })
            .collect();
    });
}

#[unsafe(no_mangle)]
extern "C" fn loom_rt_process_arg_count() -> i64 {
    #[cfg(not(windows))]
    {
        i64::from(PROCESS_ARGS.get().0)
    }
    #[cfg(windows)]
    {
        PROCESS_ARGS.with(|arguments| arguments.borrow().len() as i64)
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_process_arg_text(index: i64) -> *mut u8 {
    #[cfg(not(windows))]
    {
        let (count, arguments) = PROCESS_ARGS.get();
        let index = usize::try_from(index)
            .ok()
            .filter(|index| *index < count as usize)
            .unwrap_or_else(|| fault("process argument index out of bounds"));
        // SAFETY: process_init receives argc readable, NUL-terminated C arguments
        // from main. Their storage is independent of GC and lives until exit.
        unsafe {
            let bytes = CStr::from_ptr(*arguments.add(index)).to_bytes();
            loom_rt_text_new(bytes.as_ptr(), bytes.len())
        }
    }
    #[cfg(windows)]
    {
        PROCESS_ARGS.with(|arguments| {
            let arguments = arguments.borrow();
            let argument = usize::try_from(index)
                .ok()
                .and_then(|index| arguments.get(index))
                .unwrap_or_else(|| fault("process argument index out of bounds"));
            // SAFETY: Rust-owned argument bytes outlive the managed allocation.
            unsafe { loom_rt_text_new(argument.as_ptr(), argument.len()) }
        })
    }
}

unsafe fn process_command(arguments: *const u8) -> Result<std::process::Command, i64> {
    // SAFETY: The intrinsic accepts only List[Text]. Command copies each argument
    // into OS-owned storage; this synchronous call never allocates in Loom's heap.
    let arguments = unsafe { &*arguments.cast::<List>() };
    if arguments.buffer.len == 0 {
        return Err(-2);
    }
    let argument = |index: usize| {
        // SAFETY: Each initialized List element is a valid UTF-8 Text pointer.
        unsafe {
            let text = *arguments
                .buffer
                .data
                .add(index * arguments.stride)
                .cast::<*const u8>();
            std::str::from_utf8_unchecked(text_bytes(text))
        }
    };
    // Command otherwise dispatches Windows batch files through cmd.exe.
    #[cfg(windows)]
    if std::path::Path::new(argument(0))
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("bat") || extension.eq_ignore_ascii_case("cmd")
        })
    {
        return Err(-1);
    }
    let mut command = std::process::Command::new(argument(0));
    for index in 1..arguments.buffer.len {
        command.arg(argument(index));
    }
    Ok(command)
}

fn process_status(status: std::io::Result<std::process::ExitStatus>) -> i64 {
    match status {
        Ok(status) => status.code().map_or(-3, |code| i64::from(code as u32)),
        Err(_) => -1,
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_process_run(arguments: *const u8) -> i64 {
    // SAFETY: The private ABI passes List[Text]; no managed allocation occurs.
    match unsafe { process_command(arguments) } {
        Ok(mut command) => process_status(command.status()),
        Err(status) => status,
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_process_run_input(arguments: *const u8, input: *const u8) -> i64 {
    use std::io::Write;

    // SAFETY: Both arguments remain live for this nonallocating synchronous call.
    let mut command = match unsafe { process_command(arguments) } {
        Ok(command) => command,
        Err(status) => return status,
    };
    let Ok(mut child) = command.stdin(std::process::Stdio::piped()).spawn() else {
        return -1;
    };
    // Taking stdin guarantees the pipe closes before wait, including write errors.
    // The child is reaped even if it stops reading before consuming all input.
    let written = child.stdin.take().is_some_and(|mut pipe| {
        // SAFETY: input is immutable UTF-8 Text and no Loom GC runs while writing.
        pipe.write_all(unsafe { text_bytes(input) }).is_ok()
    });
    let status = child.wait();
    if written { process_status(status) } else { -1 }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_process_capture(
    arguments: *const u8,
    stdout: *mut u8,
    stderr: *mut u8,
) -> i64 {
    // SAFETY: Command copies literal argv before any managed allocation. Reader
    // threads own only Rust storage; all Loom heap access stays on this thread.
    let command = match unsafe { process_command(arguments) } {
        Ok(command) => command,
        Err(status) => return status,
    };
    let output = match process_io::capture(command) {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::OutOfMemory => fault("out of memory"),
        Err(_) => return -1,
    };
    rooted([stdout, stderr], |slots| {
        for (index, bytes) in [&output.stdout, &output.stderr].into_iter().enumerate() {
            if !bytes.is_empty() {
                // SAFETY: Both headers remain rooted across each reserve. Input
                // bytes are Rust-owned; reserve returns the relocated header.
                unsafe {
                    let buffer = reserve(*slots.add(index), bytes.len(), 1);
                    ptr::copy_nonoverlapping(
                        bytes.as_ptr(),
                        (*buffer).data.add((*buffer).len),
                        bytes.len(),
                    );
                    (*buffer).len += bytes.len();
                }
            }
        }
    });
    process_status(Ok(output.status))
}

#[unsafe(no_mangle)]
extern "C" fn loom_rt_process_exit(code: i64) -> ! {
    let code = u8::try_from(code).unwrap_or_else(|_| fault("exit code out of range"));
    std::process::exit(i32::from(code))
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Buffer {
    len: usize,
    cap: usize,
    data: *mut u8,
}

#[repr(C)]
struct List {
    buffer: Buffer,
    stride: usize,
    trace_element: Option<Trace>,
}

unsafe extern "C" fn trace_bytes(pointer: *mut u8) {
    // SAFETY: The collector calls this only for a live Bytes header.
    unsafe { trace_pointer(ptr::addr_of_mut!((*pointer.cast::<Buffer>()).data).cast()) };
}

unsafe extern "C" fn trace_list(pointer: *mut u8) {
    // SAFETY: The collector calls this only for a live List header, and each
    // initialized element has the layout described by the generated callback.
    unsafe {
        let list = pointer.cast::<List>();
        trace_pointer(ptr::addr_of_mut!((*list).buffer.data).cast());
        if let Some(trace) = (*list).trace_element {
            for index in 0..(*list).buffer.len {
                trace((*list).buffer.data.add(index * (*list).stride));
            }
        }
    }
}

unsafe fn reserve(owner: *mut u8, additional: usize, stride: usize) -> *mut Buffer {
    // SAFETY: Both Bytes and List begin with Buffer. Root the owner, not an
    // interior descriptor, and return its current location after any collection.
    let old = unsafe { *owner.cast::<Buffer>() };
    let required = old
        .len
        .checked_add(additional)
        .unwrap_or_else(|| fault("buffer size overflow"));
    if required <= old.cap {
        return owner.cast();
    }
    let capacity = required.max(old.cap.saturating_mul(2)).max(8);
    let size = capacity
        .checked_mul(stride)
        .unwrap_or_else(|| fault("buffer size overflow"));
    rooted([owner], |slots| {
        let data = if old.data.is_null() {
            allocate_storage(size, None, false)
        } else {
            let layout = Layout::from_size_align(size.max(1), 16)
                .unwrap_or_else(|_| fault("allocation size overflow"));
            let collect = HEAP.with(|heap| {
                let heap = heap.borrow();
                let previous = &heap.objects[&(old.data as usize)];
                heap.stress
                    || heap.bytes.saturating_add(previous.growth_cost(layout)) >= heap.threshold
            });
            if collect {
                loom_rt_collect();
            }
            // SAFETY: Collection rewrote the owner and its backing storage.
            let current = unsafe { (*(*slots).cast::<Buffer>()).data };
            HEAP.with(|heap| {
                let mut heap = heap.borrow_mut();
                let previous = heap
                    .objects
                    .remove(&(current as usize))
                    .expect("live buffer allocation");
                let pointer = if previous.individually_owned() {
                    // SAFETY: Only the rooted shared header owns this separate
                    // allocation. Growth never changes it back to an arena slot.
                    NonNull::new(unsafe { realloc(current, previous.layout(), layout.size()) })
                        .unwrap_or_else(|| fault("out of memory"))
                } else {
                    let pointer = storage(&mut heap.space, layout, false);
                    // SAFETY: Arena slices cannot be reallocated individually.
                    // Copy initialized elements; the abandoned slice dies with
                    // its arena. No collection occurs before header publication.
                    unsafe {
                        ptr::copy_nonoverlapping(current, pointer.as_ptr(), old.len * stride)
                    };
                    pointer
                };
                heap.bytes += previous.growth_cost(layout);
                heap.objects.insert(
                    pointer.as_ptr() as usize,
                    Allocation {
                        size: layout.size(),
                        trace: None,
                        forwarded: ptr::null_mut(),
                    },
                );
                pointer.as_ptr()
            })
        };
        // SAFETY: Reallocation preserves initialized elements. The shared header is
        // published before another allocation/collection; spare capacity stays raw.
        unsafe {
            let buffer = (*slots).cast::<Buffer>();
            (*buffer).data = data;
            (*buffer).cap = capacity;
            buffer
        }
    })
}

#[unsafe(no_mangle)]
extern "C" fn loom_rt_bytes_new() -> *mut u8 {
    allocate(size_of::<Buffer>(), Some(trace_bytes))
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_bytes_reserve_one(bytes: *mut u8) {
    // SAFETY: Generated code roots the owning header across this slow path.
    unsafe { reserve(bytes, 1, 1) };
}

unsafe fn buffer_bytes<'a>(bytes: *const u8) -> &'a [u8] {
    // SAFETY: The live Bytes descriptor owns len initialized bytes. Empty
    // buffers need not allocate storage and cannot form a null Rust slice.
    unsafe {
        let buffer = &*bytes.cast::<Buffer>();
        if buffer.len == 0 {
            &[]
        } else {
            std::slice::from_raw_parts(buffer.data, buffer.len)
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_bytes_utf8(bytes: *const u8) -> i32 {
    // SAFETY: bytes is a live Bytes header.
    i32::from(std::str::from_utf8(unsafe { buffer_bytes(bytes) }).is_ok())
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_bytes_text_copy(bytes: *const u8) -> *mut u8 {
    rooted([bytes.cast_mut()], |slots| {
        // SAFETY: End the validation borrow before allocating independent Text.
        // Reconstruct the buffer slice from its rewritten owner afterwards.
        unsafe {
            if std::str::from_utf8(buffer_bytes(*slots)).is_err() {
                fault("invalid UTF-8 text");
            }
            let len = (*(*slots).cast::<Buffer>()).len;
            let result = allocate_text(len);
            ptr::copy_nonoverlapping(
                buffer_bytes(*slots).as_ptr(),
                (*result).data.as_mut_ptr(),
                len,
            );
            result.cast()
        }
    })
}

#[unsafe(no_mangle)]
extern "C" fn loom_rt_list_new(stride: usize, trace_element: Option<Trace>) -> *mut u8 {
    let list = allocate(size_of::<List>(), Some(trace_list)).cast::<List>();
    // SAFETY: The allocation is a zeroed header and no GC occurs before return.
    unsafe {
        (*list).stride = stride;
        (*list).trace_element = trace_element;
    }
    list.cast()
}

unsafe fn list_push(list: *mut u8, item: *const u8) {
    let list = list.cast::<List>();
    // SAFETY: The caller roots list and managed references in item. This private
    // helper takes an independent stack item, never a pointer into the buffer.
    unsafe {
        let stride = (*list).stride;
        let list = reserve(list.cast(), 1, stride).cast::<List>();
        ptr::copy(
            item,
            (*list).buffer.data.add((*list).buffer.len * stride),
            stride,
        );
        (*list).buffer.len += 1;
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_list_reserve_one(list: *mut u8) {
    // SAFETY: Generated code roots the header and its pending typed item. The
    // caller reloads the backing pointer and publishes length after storing it.
    unsafe {
        let list = list.cast::<List>();
        reserve(list.cast(), 1, (*list).stride);
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_file_open(path: *const u8) -> i64 {
    // SAFETY: Text is valid UTF-8; the platform operation does not retain it.
    file_io::open(
        unsafe { std::str::from_utf8_unchecked(text_bytes(path)) },
        false,
    )
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_file_create(path: *const u8) -> i64 {
    // SAFETY: Text is valid UTF-8; the platform operation does not retain it.
    file_io::open(
        unsafe { std::str::from_utf8_unchecked(text_bytes(path)) },
        true,
    )
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_file_write(fd: i64, text: *const u8, offset: i64) -> i64 {
    let Ok(offset) = usize::try_from(offset) else {
        return -1;
    };
    // SAFETY: text is live for this non-retaining, nonallocating call.
    let bytes = unsafe { text_bytes(text) };
    let Some(bytes) = bytes.get(offset..) else {
        return -1;
    };
    file_io::write(fd, bytes)
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_file_write_bytes(fd: i64, bytes: *const u8, offset: i64) -> i64 {
    let Ok(offset) = usize::try_from(offset) else {
        return -1;
    };
    // SAFETY: The Bytes header and storage remain live for this non-retaining,
    // nonallocating call. Binary contents do not require UTF-8 validation.
    let bytes = unsafe { buffer_bytes(bytes) };
    let Some(bytes) = bytes.get(offset..) else {
        return -1;
    };
    file_io::write(fd, bytes)
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_file_read(fd: i64, bytes: *mut u8, limit: i64) -> i64 {
    let Ok(limit) = usize::try_from(limit) else {
        return -1;
    };
    if limit == 0 {
        return 0;
    }
    // SAFETY: Caller roots bytes. reserve provides limit writable bytes beyond
    // len, and read initializes exactly its nonnegative result count.
    unsafe {
        let buffer = reserve(bytes, limit, 1);
        // The Rust Read boundary takes initialized bytes, unlike typed push.
        // Initialize only this requested I/O range, not all spare capacity.
        ptr::write_bytes((*buffer).data.add((*buffer).len), 0, limit);
        let tail = std::slice::from_raw_parts_mut((*buffer).data.add((*buffer).len), limit);
        let count = file_io::read(fd, tail);
        if count > 0 {
            (*buffer).len += count as usize;
        }
        count
    }
}

#[unsafe(no_mangle)]
extern "C" fn loom_rt_file_close(fd: i64) -> i64 {
    file_io::close(fd)
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_directory_read(path: *const u8, names: *mut u8) -> i64 {
    // SAFETY: The caller supplies rooted Text and List[Text] values. Text is
    // valid UTF-8; read_dir consumes the path without retaining its bytes.
    let entries = {
        let path = unsafe { std::str::from_utf8_unchecked(text_bytes(path)) };
        std::fs::read_dir(path)
    };
    let Ok(entries) = entries else {
        return -1;
    };
    rooted([names, ptr::null_mut()], |slots| {
        for entry in entries {
            let Ok(entry) = entry else { return -1 };
            let spelling = entry.file_name();
            let Some(spelling) = spelling.to_str() else {
                return -2;
            };
            // SAFETY: Owned OS bytes survive the Text allocation; the name
            // root protects its copy through any list backing-buffer growth.
            unsafe {
                *slots.add(1) = loom_rt_text_new(spelling.as_ptr(), spelling.len());
                list_push(*slots, slots.add(1).cast());
            }
        }
        0
    })
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_path_kind(path: *const u8) -> i64 {
    // SAFETY: The non-retaining call receives valid UTF-8 Text. Metadata follows
    // symlinks; a missing target is reported as an error, not as a file kind.
    let path = unsafe { std::str::from_utf8_unchecked(text_bytes(path)) };
    let Ok(metadata) = std::fs::metadata(path) else {
        return -1;
    };
    if metadata.is_file() {
        0
    } else if metadata.is_dir() {
        1
    } else {
        2
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_path_canonical(path: *const u8, bytes: *mut u8) -> i64 {
    // SAFETY: The caller roots the Text and mutable Bytes arguments.
    let canonical = {
        let path = unsafe { std::str::from_utf8_unchecked(text_bytes(path)) };
        std::fs::canonicalize(path)
    };
    let Ok(canonical) = canonical else {
        return -1;
    };
    let Some(spelling) = canonical.to_str() else {
        return -2;
    };
    // SAFETY: reserve may collect but the caller roots bytes and the canonical
    // path owns its spelling separately. The reserved tail holds every byte.
    unsafe {
        let buffer = reserve(bytes, spelling.len(), 1);
        ptr::copy_nonoverlapping(
            spelling.as_ptr(),
            (*buffer).data.add((*buffer).len),
            spelling.len(),
        );
        (*buffer).len += spelling.len();
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live() -> usize {
        HEAP.with(|heap| heap.borrow().objects.len())
    }

    struct TestRoot(Box<(RootFrame, Root)>);

    impl Drop for TestRoot {
        fn drop(&mut self) {
            // SAFETY: Box keeps the frame/entry stable; tests release guards in
            // reverse registration order before the addressed slot expires.
            unsafe { loom_rt_roots_leave(ptr::from_mut(&mut self.0.0)) };
        }
    }

    fn root(address: *mut *mut u8) -> TestRoot {
        let mut storage = Box::new((
            RootFrame {
                previous: ptr::null_mut(),
                roots: ptr::null(),
                count: 0,
            },
            Root {
                address: address.cast(),
                trace: trace_pointer,
            },
        ));
        // SAFETY: Box keeps both entries stable when this helper returns. The
        // tests keep the addressed stack slot live until dropping the guard.
        unsafe { loom_rt_roots_enter(ptr::from_mut(&mut storage.0), ptr::from_ref(&storage.1), 1) };
        TestRoot(storage)
    }

    fn text(value: &str) -> *mut u8 {
        // SAFETY: The source bytes live throughout the non-retaining copy call.
        unsafe { loom_rt_text_new(value.as_ptr(), value.len()) }
    }

    unsafe fn bytes_push(bytes: *mut u8, byte: u8) {
        // SAFETY: The helper and caller both keep rewritable pointer slots.
        // Reload this by-value input after reserve may relocate the header.
        rooted([bytes], |slots| unsafe {
            loom_rt_bytes_reserve_one(*slots);
            let buffer = (*slots).cast::<Buffer>();
            *(*buffer).data.add((*buffer).len) = byte;
            (*buffer).len += 1;
        });
    }

    #[test]
    fn process_capture_appends_binary_outputs_and_reloads_shared_buffers() {
        use process_io::{TEST_BYTES, TEST_NAME, TEST_TOKEN};
        let executable = std::env::current_exe().unwrap();
        rooted([ptr::null_mut(); 4], |slots| unsafe {
            *slots = loom_rt_list_new(size_of::<*mut u8>(), Some(trace_pointer));
            *slots.add(1) = loom_rt_bytes_new();
            *slots.add(2) = loom_rt_bytes_new();
            bytes_push(*slots.add(1), b'!');
            bytes_push(*slots.add(2), b'?');
            assert_eq!(
                loom_rt_process_capture(*slots, *slots.add(1), *slots.add(2)),
                -2
            );
            *slots.add(3) = text("invalid\0executable");
            list_push(*slots, slots.add(3).cast());
            assert_eq!(
                loom_rt_process_capture(*slots, *slots.add(1), *slots.add(2)),
                -1
            );
            assert_eq!(buffer_bytes(*slots.add(1)), b"!");
            assert_eq!(buffer_bytes(*slots.add(2)), b"?");
            *slots = loom_rt_list_new(size_of::<*mut u8>(), Some(trace_pointer));
            for argument in [
                executable.to_str().unwrap(),
                "--ignored",
                "--exact",
                TEST_NAME,
                "--nocapture",
                "--skip",
                TEST_TOKEN,
            ] {
                *slots.add(3) = text(argument);
                list_push(*slots, slots.add(3).cast());
            }
            *slots.add(3) = *slots.add(1);
            let previous = *slots.add(1) as usize;
            HEAP.with(|heap| heap.borrow_mut().threshold = 0);
            assert_eq!(
                loom_rt_process_capture(*slots, *slots.add(1), *slots.add(2)),
                7
            );
            assert_ne!(*slots.add(1) as usize, previous);
            assert_eq!(*slots.add(1), *slots.add(3));
            let stdout = (0..TEST_BYTES)
                .map(|index| (index % 256) as u8)
                .collect::<Vec<_>>();
            let stderr = (0..TEST_BYTES)
                .map(|index| (255 - index % 256) as u8)
                .collect::<Vec<_>>();
            assert_eq!(buffer_bytes(*slots.add(1))[0], b'!');
            assert!(buffer_bytes(*slots.add(1)).ends_with(&stdout));
            assert_eq!(&buffer_bytes(*slots.add(2))[1..], stderr);
        });
        loom_rt_collect();
        assert_eq!(live(), 0);
    }

    #[test]
    fn float_codecs_roundtrip_ieee_values_across_collection() {
        let mut formatted = ptr::null_mut();
        let checkpoint = root(ptr::addr_of_mut!(formatted));
        for value in [
            0.1,
            0.0,
            -0.0,
            f64::MIN,
            f64::MAX,
            f64::MIN_POSITIVE,
            f64::from_bits(1),
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NAN,
        ] {
            HEAP.with(|heap| heap.borrow_mut().threshold = 0);
            formatted = loom_rt_float_format(value);
            HEAP.with(|heap| heap.borrow_mut().threshold = 0);
            let _garbage = loom_rt_float_format(42.5);
            // SAFETY: formatted stays rooted across the second formatting call.
            let parsed = unsafe { loom_rt_float_parse(formatted) };
            if value.is_nan() {
                assert!(parsed.is_nan());
            } else {
                assert_eq!(parsed.to_bits(), value.to_bits());
            }
            if value.to_bits() == (-0.0_f64).to_bits() {
                // SAFETY: The same root keeps the formatted Text readable.
                assert_eq!(unsafe { text_bytes(formatted) }, b"-0");
            }
        }
        drop(checkpoint);
        loom_rt_collect();
        assert_eq!(live(), 0);
    }

    #[test]
    fn roots_preserve_live_objects_and_static_literals_need_no_heap_entry() {
        let mut kept = text("kept");
        let checkpoint = root(ptr::addr_of_mut!(kept));
        let _garbage = text("garbage");
        #[repr(C)]
        struct Literal {
            len: usize,
            bytes: [u8; 3],
        }
        let literal = Literal {
            len: 3,
            bytes: *b"abc",
        };
        let mut static_text = ptr::from_ref(&literal).cast_mut().cast();
        let literal_root = root(ptr::addr_of_mut!(static_text));
        let previous = kept as usize;
        loom_rt_collect();
        assert_eq!(live(), 1);
        assert_ne!(kept as usize, previous);
        assert_eq!(static_text, ptr::from_ref(&literal).cast_mut().cast());
        // SAFETY: kept is an active root and literal has the required Text layout.
        unsafe {
            assert_eq!(text_bytes(kept).len(), 4);
            assert_eq!(text_bytes(static_text)[1], 98);
        }
        drop(literal_root);
        drop(checkpoint);
        loom_rt_collect();
        assert_eq!(live(), 0);
    }

    #[test]
    fn shared_list_growth_traces_elements_and_reclaims_old_buffers() {
        let mut list = loom_rt_list_new(size_of::<*mut u8>(), Some(trace_pointer));
        let checkpoint = root(ptr::addr_of_mut!(list));
        let mut alias = list;
        let alias_root = root(ptr::addr_of_mut!(alias));
        let mut item = ptr::null_mut();
        let temporary = root(ptr::addr_of_mut!(item));
        for index in 0..40 {
            HEAP.with(|heap| heap.borrow_mut().threshold = 0);
            item = text(&index.to_string());
            HEAP.with(|heap| heap.borrow_mut().threshold = 0);
            // SAFETY: Both the shared header and item slot are rooted.
            unsafe { list_push(list, ptr::addr_of!(item).cast()) };
        }
        // SAFETY: The list's own rooted slot supplies a traced self-cycle.
        unsafe { list_push(list, ptr::addr_of!(list).cast()) };
        drop(temporary);
        // SAFETY: Grow only capacity; the existing initialized elements stay live.
        unsafe {
            reserve(
                list,
                LARGE_OBJECT_THRESHOLD / size_of::<*mut u8>(),
                size_of::<*mut u8>(),
            )
        };
        let previous_data = unsafe { (*list.cast::<List>()).buffer.data };
        let previous = list as usize;
        loom_rt_collect();
        assert_eq!(live(), 42); // One header, one buffer, forty Text objects.
        assert_ne!(list as usize, previous);
        assert_eq!(list, alias);
        // SAFETY: alias refers to the same live header as list.
        unsafe {
            let buffer = &(*alias.cast::<List>()).buffer;
            assert_eq!(
                buffer.data == previous_data,
                HEAP.with(|heap| !heap.borrow().stress)
            );
            assert_eq!(buffer.len, 41);
            let last = *buffer.data.cast::<*mut u8>().add(39);
            assert_eq!(text_bytes(last), b"39");
            assert_eq!(*buffer.data.cast::<*mut u8>().add(40), list);
        }
        drop(alias_root);
        drop(checkpoint);
        loom_rt_collect();
        assert_eq!(live(), 0);
    }

    #[test]
    fn bytes_to_text_is_validated_and_independent() {
        let mut bytes = loom_rt_bytes_new();
        let checkpoint = root(ptr::addr_of_mut!(bytes));
        // SAFETY: The root remains active for every allocation in this test.
        unsafe {
            for byte in "hé".bytes() {
                HEAP.with(|heap| heap.borrow_mut().threshold = 0);
                bytes_push(bytes, byte);
            }
            assert_eq!((*bytes.cast::<Buffer>()).len, 3);
            assert_eq!(loom_rt_bytes_utf8(bytes), 1);
            let previous = bytes as usize;
            HEAP.with(|heap| heap.borrow_mut().threshold = 0);
            let mut copied = loom_rt_bytes_text_copy(bytes);
            let copied_root = root(ptr::addr_of_mut!(copied));
            assert_ne!(bytes as usize, previous);
            let capacity = (*bytes.cast::<Buffer>()).cap;
            HEAP.with(|heap| heap.borrow_mut().threshold = 0);
            bytes_push(bytes, 255);
            while (*bytes.cast::<Buffer>()).len <= capacity {
                bytes_push(bytes, 0); // Grow the shared buffer after taking the copy.
            }
            HEAP.with(|heap| {
                let heap = heap.borrow();
                let live = heap
                    .objects
                    .values()
                    .map(|object| occupied(object.layout()))
                    .sum::<usize>();
                let abandoned = occupied(Layout::from_size_align(capacity, 16).unwrap());
                assert_eq!(heap.bytes, live + abandoned);
            });
            assert_eq!(loom_rt_bytes_utf8(bytes), 0);
            assert_eq!(text_bytes(copied), "hé".as_bytes());
            HEAP.with(|heap| heap.borrow_mut().threshold = 0);
            let mut joined = loom_rt_text_concat(copied, copied);
            let joined_root = root(ptr::addr_of_mut!(joined));
            let previous = joined as usize;
            loom_rt_collect();
            assert_ne!(joined as usize, previous);
            assert_eq!(text_bytes(joined), "héhé".as_bytes());
            assert_eq!(loom_rt_text_equal(copied, copied), 1);
            drop(joined_root);
            drop(copied_root);
        }
        drop(checkpoint);
        loom_rt_collect();
        assert_eq!(live(), 0);
    }

    #[test]
    fn text_slices_copy_utf8_ranges_and_unicode_queries_validate_scalars() {
        let mut source = text("A界🙂Z");
        let checkpoint = root(ptr::addr_of_mut!(source));
        let previous = source as usize;
        HEAP.with(|heap| heap.borrow_mut().threshold = 0);
        // SAFETY: source and slice stay rooted across all copying allocations.
        unsafe {
            let mut slice = loom_rt_text_slice(source, 1, 8);
            let slice_root = root(ptr::addr_of_mut!(slice));
            assert_ne!(source as usize, previous);
            assert_eq!(text_bytes(slice), "界🙂".as_bytes());
            assert_ne!(slice, source);
            HEAP.with(|heap| heap.borrow_mut().threshold = 0);
            assert_eq!(text_bytes(loom_rt_text_slice(slice, 3, 7)), "🙂".as_bytes());
            HEAP.with(|heap| heap.borrow_mut().threshold = 0);
            assert_eq!(text_bytes(loom_rt_text_slice(source, 4, 4)), b"");
            drop(slice_root);
        }
        assert_eq!(loom_rt_unicode_alphabetic(i64::from('界' as u32)), 1);
        assert_eq!(loom_rt_unicode_alphanumeric(i64::from('٣' as u32)), 1);
        assert_eq!(loom_rt_unicode_whitespace(0x2003), 1);
        assert_eq!(loom_rt_unicode_alphabetic(i64::from(b'3')), 0);
        for invalid in [-1, 0xd800, 0x110000, i64::MAX] {
            assert_eq!(loom_rt_unicode_alphabetic(invalid), 0);
            assert_eq!(loom_rt_unicode_alphanumeric(invalid), 0);
            assert_eq!(loom_rt_unicode_whitespace(invalid), 0);
        }
        drop(checkpoint);
        loom_rt_collect();
        assert_eq!(live(), 0);
    }

    #[test]
    #[cfg(not(windows))]
    fn process_arguments_are_copied_from_live_argv() {
        let previous = PROCESS_ARGS.get();
        let owned = [
            std::ffi::CString::new("loom").unwrap(),
            std::ffi::CString::new("hé.loom").unwrap(),
        ];
        let arguments = owned.each_ref().map(|argument| argument.as_ptr());
        // SAFETY: argv and both CStrings remain alive through all argument reads.
        unsafe { loom_rt_process_init(arguments.len() as i32, arguments.as_ptr()) };
        assert_eq!(loom_rt_process_arg_count(), 2);
        // SAFETY: Both requested indexes are within the live argv descriptor.
        unsafe {
            let mut executable = loom_rt_process_arg_text(0);
            let checkpoint = root(ptr::addr_of_mut!(executable));
            HEAP.with(|heap| heap.borrow_mut().threshold = 0);
            let argument = loom_rt_process_arg_text(1);
            assert_eq!(text_bytes(argument), "hé.loom".as_bytes());
            assert_eq!(text_bytes(executable), b"loom");
            drop(checkpoint);
            loom_rt_process_init(0, ptr::null());
        }
        assert_eq!(loom_rt_process_arg_count(), 0);
        PROCESS_ARGS.set(previous);
        loom_rt_collect();
        assert_eq!(live(), 0);
    }

    #[test]
    #[cfg(windows)]
    fn process_arguments_use_native_unicode_not_narrow_argv() {
        let previous = PROCESS_ARGS.with(|arguments| arguments.borrow().clone());
        // SAFETY: Windows ignores the narrow argv after validating its shape.
        unsafe { loom_rt_process_init(0, ptr::null()) };
        assert_eq!(
            loom_rt_process_arg_count(),
            std::env::args_os().count() as i64
        );
        PROCESS_ARGS.with(|arguments| {
            *arguments.borrow_mut() = vec!["loom".into(), "目录-é-🙂.loom".into()]
        });
        // SAFETY: The Unicode argument is owned independently of managed GC.
        unsafe {
            assert_eq!(
                text_bytes(loom_rt_process_arg_text(1)),
                "目录-é-🙂.loom".as_bytes()
            )
        };
        PROCESS_ARGS.with(|arguments| *arguments.borrow_mut() = previous);
        loom_rt_collect();
        assert_eq!(live(), 0);
    }

    #[test]
    fn files_are_read_incrementally_and_closed_explicitly() {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"loom").unwrap();
        let mut path = text(file.path().to_str().unwrap());
        let checkpoint = root(ptr::addr_of_mut!(path));
        let mut bytes = loom_rt_bytes_new();
        let bytes_root = root(ptr::addr_of_mut!(bytes));
        // SAFETY: Text and Bytes headers remain rooted and descriptors are only
        // used synchronously. Negative descriptors cannot name another resource.
        unsafe {
            let fd = loom_rt_file_open(path);
            assert!(fd >= 0);
            let previous = bytes as usize;
            HEAP.with(|heap| heap.borrow_mut().threshold = 0);
            assert_eq!(loom_rt_file_read(fd, bytes, 2), 2);
            assert_ne!(bytes as usize, previous);
            loom_rt_collect();
            assert_eq!(loom_rt_file_read(fd, bytes, 2), 2);
            HEAP.with(|heap| heap.borrow_mut().threshold = 0);
            assert_eq!(loom_rt_file_read(fd, bytes, 2), 0);
            assert_eq!(buffer_bytes(bytes), b"loom");
            assert_eq!(loom_rt_file_close(fd), 0);
            assert_eq!(loom_rt_file_read(-1, bytes, 1), -1);
            assert_eq!(loom_rt_file_close(-1), -1);

            let mut output = text("prefix:ok");
            let output_root = root(ptr::addr_of_mut!(output));
            let fd = loom_rt_file_create(path);
            assert!(fd >= 0);
            assert_eq!(loom_rt_file_write(fd, output, -1), -1);
            assert_eq!(loom_rt_file_write(fd, output, 10), -1);
            assert_eq!(loom_rt_file_write(-1, output, 0), -1);
            assert_eq!(loom_rt_file_write(fd, output, 7), 2);
            assert_eq!(loom_rt_file_close(fd), 0);
            bytes = loom_rt_bytes_new();
            let fd = loom_rt_file_open(path);
            assert!(fd >= 0);
            HEAP.with(|heap| heap.borrow_mut().threshold = 0);
            assert_eq!(loom_rt_file_read(fd, bytes, 16), 2);
            assert_eq!(buffer_bytes(bytes), b"ok");
            assert_eq!(loom_rt_file_close(fd), 0);
            drop(output_root);

            bytes = loom_rt_bytes_new();
            for byte in [b'x', 0, 255, 128, b'\n'] {
                HEAP.with(|heap| heap.borrow_mut().threshold = 0);
                bytes_push(bytes, byte);
            }
            loom_rt_collect();
            let fd = loom_rt_file_create(path);
            assert!(fd >= 0);
            assert_eq!(loom_rt_file_write_bytes(fd, bytes, -1), -1);
            assert_eq!(loom_rt_file_write_bytes(fd, bytes, 6), -1);
            assert_eq!(loom_rt_file_write_bytes(-1, bytes, 0), -1);
            assert_eq!(loom_rt_file_write_bytes(fd, bytes, 1), 4);
            assert_eq!(loom_rt_file_write_bytes(fd, bytes, 5), 0);
            assert_eq!(loom_rt_file_close(fd), 0);
            assert_eq!(std::fs::read(file.path()).unwrap(), [0, 255, 128, b'\n']);

            let missing = file.path().with_extension("missing");
            path = text(missing.to_str().unwrap());
            assert_eq!(loom_rt_file_open(path), -1);
        }
        drop(bytes_root);
        drop(checkpoint);
        loom_rt_collect();
        assert_eq!(live(), 0);
    }
}
