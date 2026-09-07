//! Loom's single-threaded, nonmoving managed-memory and platform boundary.
//! Generated code roots every live managed slot before an allocating call.

use std::alloc::{Layout, alloc, alloc_zeroed, dealloc, realloc};
#[cfg(not(windows))]
use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashMap;
#[cfg(not(windows))]
use std::ffi::CStr;
use std::ffi::c_char;
use std::ptr::{self, NonNull};

mod file_io;

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
    loom_rt_mark(unsafe { *address.cast::<*mut u8>() });
}

struct Allocation {
    pointer: NonNull<u8>,
    layout: Layout,
    trace: Option<Trace>,
    marked: bool,
}

struct Heap {
    objects: HashMap<usize, Allocation>,
    roots: *mut RootFrame,
    work: Vec<(*mut u8, Trace)>,
    bytes: usize,
    threshold: usize,
    stress: bool,
}

const MIN_THRESHOLD: usize = 64 * 1024;

impl Default for Heap {
    fn default() -> Self {
        Self {
            objects: HashMap::new(),
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
        for allocation in self.objects.values() {
            // SAFETY: Each entry owns one allocation with its original layout.
            unsafe { dealloc(allocation.pointer.as_ptr(), allocation.layout) };
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

fn allocate_storage(size: usize, trace: Option<Trace>, zeroed: bool) -> *mut u8 {
    let layout = Layout::from_size_align(size.max(1), 16)
        .unwrap_or_else(|_| fault("allocation size overflow"));
    let collect = HEAP.with(|heap| {
        let heap = heap.borrow();
        heap.stress || heap.bytes.saturating_add(layout.size()) >= heap.threshold
    });
    if collect {
        loom_rt_collect();
    }
    // SAFETY: The layout is valid and every allocation is eventually swept or
    // freed by Heap::drop. Headers/payloads start zeroed; raw buffer capacity is
    // never traced/read beyond its published initialized length.
    let pointer = NonNull::new(unsafe {
        if zeroed {
            alloc_zeroed(layout)
        } else {
            alloc(layout)
        }
    })
    .unwrap_or_else(|| fault("out of memory"));
    HEAP.with(|heap| {
        let mut heap = heap.borrow_mut();
        heap.bytes += layout.size();
        heap.objects.insert(
            pointer.as_ptr() as usize,
            Allocation {
                pointer,
                layout,
                trace,
                marked: false,
            },
        );
    });
    pointer.as_ptr()
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
extern "C" fn loom_rt_mark(pointer: *mut u8) {
    HEAP.with(|heap| {
        let mut heap = heap.borrow_mut();
        let Some(object) = heap.objects.get_mut(&(pointer as usize)) else {
            return; // Null and static Text literals are not heap allocations.
        };
        if !object.marked {
            object.marked = true;
            let trace = object.trace;
            if let Some(trace) = trace {
                heap.work.push((pointer, trace));
            }
        }
    });
}

#[unsafe(no_mangle)]
extern "C" fn loom_rt_collect() {
    let mut frame = HEAP.with(|heap| {
        let mut heap = heap.borrow_mut();
        heap.work.clear();
        for object in heap.objects.values_mut() {
            object.marked = false;
        }
        heap.roots
    });
    while !frame.is_null() {
        // SAFETY: This synchronous, nonmoving collector sees stable active
        // frames and slots. Tracers only mark objects; they cannot allocate,
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
        // SAFETY: The marked allocation is live. No heap borrow spans callbacks,
        // which may mark more objects but must not allocate or collect.
        unsafe { trace(pointer) };
    }
    HEAP.with(|heap| {
        let mut heap = heap.borrow_mut();
        let mut reclaimed = 0;
        heap.objects.retain(|_, object| {
            if !object.marked {
                reclaimed += object.layout.size();
                // SAFETY: Unreachable managed memory has no remaining roots.
                unsafe { dealloc(object.pointer.as_ptr(), object.layout) };
            }
            object.marked
        });
        heap.bytes -= reclaimed;
        heap.threshold = heap.bytes.saturating_mul(2).max(MIN_THRESHOLD);
    });
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

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_text_new(bytes: *const u8, len: usize) -> *mut u8 {
    // SAFETY: The input buffer is readable for len bytes and stays rooted across
    // allocation when managed. A zero-length buffer may be null.
    let bytes = if len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(bytes, len) }
    };
    if std::str::from_utf8(bytes).is_err() {
        fault("invalid UTF-8 text");
    }
    let size = size_of::<Text>()
        .checked_add(len)
        .unwrap_or_else(|| fault("text size overflow"));
    let result = allocate(size, None).cast::<Text>();
    // SAFETY: result has space for the header and bytes; its storage is disjoint.
    unsafe {
        (*result).len = len;
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
    // SAFETY: Both inputs are valid UTF-8 and remain rooted by the caller.
    let (left, right) = unsafe { (text_bytes(left), text_bytes(right)) };
    let len = left
        .len()
        .checked_add(right.len())
        .unwrap_or_else(|| fault("text size overflow"));
    let size = size_of::<Text>()
        .checked_add(len)
        .unwrap_or_else(|| fault("text size overflow"));
    let result = allocate(size, None).cast::<Text>();
    // SAFETY: The new allocation is disjoint and large enough for both slices.
    unsafe {
        (*result).len = len;
        let data = (*result).data.as_mut_ptr();
        ptr::copy_nonoverlapping(left.as_ptr(), data, left.len());
        ptr::copy_nonoverlapping(right.as_ptr(), data.add(left.len()), right.len());
    }
    result.cast()
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_text_equal(left: *const u8, right: *const u8) -> i32 {
    // SAFETY: Both pointers denote live Text objects or static literals.
    i32::from(unsafe { text_bytes(left) == text_bytes(right) })
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_text_slice(text: *const u8, start: i64, end: i64) -> *mut u8 {
    // SAFETY: Text is valid UTF-8 and its caller roots it across the copy.
    let text = unsafe { std::str::from_utf8_unchecked(text_bytes(text)) };
    let range = usize::try_from(start)
        .ok()
        .zip(usize::try_from(end).ok())
        .and_then(|(start, end)| text.get(start..end))
        .unwrap_or_else(|| fault("invalid text slice range or UTF-8 boundary"));
    // SAFETY: The validated range remains live through the rooted source Text.
    unsafe { loom_rt_text_new(range.as_ptr(), range.len()) }
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
    loom_rt_mark(unsafe { (*pointer.cast::<Buffer>()).data });
}

unsafe extern "C" fn trace_list(pointer: *mut u8) {
    // SAFETY: The collector calls this only for a live List header, and each
    // initialized element has the layout described by the generated callback.
    unsafe {
        let list = &*pointer.cast::<List>();
        loom_rt_mark(list.buffer.data);
        if let Some(trace) = list.trace_element {
            for index in 0..list.buffer.len {
                trace(list.buffer.data.add(index * list.stride));
            }
        }
    }
}

unsafe fn reserve(buffer: *mut Buffer, additional: usize, stride: usize) {
    // SAFETY: The caller roots the owning header. Copying its descriptor avoids
    // holding a mutable reference while GC traces that same header.
    let old = unsafe { *buffer };
    let required = old
        .len
        .checked_add(additional)
        .unwrap_or_else(|| fault("buffer size overflow"));
    if required <= old.cap {
        return;
    }
    let capacity = required.max(old.cap.saturating_mul(2)).max(8);
    let size = capacity
        .checked_mul(stride)
        .unwrap_or_else(|| fault("buffer size overflow"));
    let data = if old.data.is_null() {
        allocate_storage(size, None, false)
    } else {
        let layout = Layout::from_size_align(size.max(1), 16)
            .unwrap_or_else(|_| fault("allocation size overflow"));
        let collect = HEAP.with(|heap| {
            let heap = heap.borrow();
            let previous = &heap.objects[&(old.data as usize)];
            heap.stress
                || heap
                    .bytes
                    .saturating_add(layout.size() - previous.layout.size())
                    >= heap.threshold
        });
        if collect {
            loom_rt_collect();
        }
        HEAP.with(|heap| {
            let mut heap = heap.borrow_mut();
            let previous = heap
                .objects
                .remove(&(old.data as usize))
                .expect("live buffer allocation");
            // SAFETY: Only the rooted shared header owns this raw allocation;
            // no source-visible interior pointer can survive a growth call.
            // No collection occurs while the allocation is being relocated.
            let pointer =
                NonNull::new(unsafe { realloc(old.data, previous.layout, layout.size()) })
                    .unwrap_or_else(|| fault("out of memory"));
            heap.bytes += layout.size() - previous.layout.size();
            heap.objects.insert(
                pointer.as_ptr() as usize,
                Allocation {
                    pointer,
                    layout,
                    trace: None,
                    marked: false,
                },
            );
            pointer.as_ptr()
        })
    };
    // SAFETY: Reallocation preserves initialized elements. The shared header is
    // published before another allocation/collection; spare capacity stays raw.
    unsafe {
        (*buffer).data = data;
        (*buffer).cap = capacity;
    }
}

#[unsafe(no_mangle)]
extern "C" fn loom_rt_bytes_new() -> *mut u8 {
    allocate(size_of::<Buffer>(), Some(trace_bytes))
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loom_rt_bytes_reserve_one(bytes: *mut u8) {
    // SAFETY: Generated code roots the owning header across this slow path.
    unsafe { reserve(bytes.cast::<Buffer>(), 1, 1) };
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
    // SAFETY: The caller roots bytes; text_new validates and creates independent
    // UTF-8 storage, so later writes through any Bytes alias cannot alter Text.
    unsafe {
        let bytes = buffer_bytes(bytes);
        loom_rt_text_new(bytes.as_ptr(), bytes.len())
    }
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
        reserve(ptr::addr_of_mut!((*list).buffer), 1, stride);
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
        reserve(ptr::addr_of_mut!((*list).buffer), 1, (*list).stride);
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
    let buffer = bytes.cast::<Buffer>();
    // SAFETY: Caller roots bytes. reserve provides limit writable bytes beyond
    // len, and read initializes exactly its nonnegative result count.
    unsafe {
        reserve(buffer, limit, 1);
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
    let path = unsafe { std::str::from_utf8_unchecked(text_bytes(path)) };
    let Ok(entries) = std::fs::read_dir(path) else {
        return -1;
    };
    let mut name: *mut u8 = ptr::null_mut();
    let root = Root {
        address: ptr::from_mut(&mut name).cast(),
        trace: trace_pointer,
    };
    // SAFETY: The slot stays live until roots_leave, including early failures
    // inside the closure. Growing names can collect the just-created Text.
    let mut frame = std::mem::MaybeUninit::<RootFrame>::uninit();
    unsafe { loom_rt_roots_enter(frame.as_mut_ptr(), &root, 1) };
    let status = (|| {
        for entry in entries {
            let Ok(entry) = entry else { return -1 };
            let spelling = entry.file_name();
            let Some(spelling) = spelling.to_str() else {
                return -2;
            };
            // SAFETY: Owned OS bytes survive the Text allocation; the name
            // root protects its copy through any list backing-buffer growth.
            unsafe {
                name = loom_rt_text_new(spelling.as_ptr(), spelling.len());
                list_push(names, ptr::from_ref(&name).cast());
            }
        }
        0
    })();
    // SAFETY: The address-taken frame and its root are still live and on top.
    unsafe { loom_rt_roots_leave(frame.as_mut_ptr()) };
    status
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
    let path = unsafe { std::str::from_utf8_unchecked(text_bytes(path)) };
    let Ok(canonical) = std::fs::canonicalize(path) else {
        return -1;
    };
    let Some(spelling) = canonical.to_str() else {
        return -2;
    };
    let buffer = bytes.cast::<Buffer>();
    // SAFETY: reserve may collect but the caller roots bytes and the canonical
    // path owns its spelling separately. The reserved tail holds every byte.
    unsafe {
        reserve(buffer, spelling.len(), 1);
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

    fn root(address: &mut *mut u8) -> TestRoot {
        let mut storage = Box::new((
            RootFrame {
                previous: ptr::null_mut(),
                roots: ptr::null(),
                count: 0,
            },
            Root {
                address: ptr::from_mut(address).cast(),
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
        // SAFETY: Tests root the header across reserve and initialize the new
        // slot before publishing its length, just like generated typed stores.
        unsafe {
            loom_rt_bytes_reserve_one(bytes);
            let buffer = bytes.cast::<Buffer>();
            *(*buffer).data.add((*buffer).len) = byte;
            (*buffer).len += 1;
        }
    }

    #[test]
    fn float_codecs_roundtrip_ieee_values_across_collection() {
        let mut formatted = ptr::null_mut();
        let checkpoint = root(&mut formatted);
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
        let checkpoint = root(&mut kept);
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
        loom_rt_mark(ptr::from_ref(&literal).cast_mut().cast());
        loom_rt_collect();
        assert_eq!(live(), 1);
        // SAFETY: kept is an active root and literal has the required Text layout.
        unsafe {
            assert_eq!(text_bytes(kept).len(), 4);
            assert_eq!(text_bytes(ptr::from_ref(&literal).cast())[1], 98);
        }
        drop(checkpoint);
        loom_rt_collect();
        assert_eq!(live(), 0);
    }

    #[test]
    fn shared_list_growth_traces_elements_and_reclaims_old_buffers() {
        let mut list = loom_rt_list_new(size_of::<*mut u8>(), Some(trace_pointer));
        let checkpoint = root(&mut list);
        let alias = list;
        let mut item = ptr::null_mut();
        let temporary = root(&mut item);
        for index in 0..40 {
            HEAP.with(|heap| heap.borrow_mut().threshold = 0);
            item = text(&index.to_string());
            HEAP.with(|heap| heap.borrow_mut().threshold = 0);
            // SAFETY: Both the shared header and item slot are rooted.
            unsafe { list_push(list, ptr::from_ref(&item).cast()) };
        }
        drop(temporary);
        loom_rt_collect();
        assert_eq!(live(), 42); // One header, one buffer, forty Text objects.
        // SAFETY: alias refers to the same live header as list.
        unsafe {
            let buffer = &(*alias.cast::<List>()).buffer;
            assert_eq!(buffer.len, 40);
            let last = *buffer.data.cast::<*mut u8>().add(39);
            assert_eq!(text_bytes(last), b"39");
        }
        drop(checkpoint);
        loom_rt_collect();
        assert_eq!(live(), 0);
    }

    #[test]
    fn bytes_to_text_is_validated_and_independent() {
        let mut bytes = loom_rt_bytes_new();
        let checkpoint = root(&mut bytes);
        // SAFETY: The root remains active for every allocation in this test.
        unsafe {
            for byte in "hé".bytes() {
                bytes_push(bytes, byte);
            }
            assert_eq!((*bytes.cast::<Buffer>()).len, 3);
            assert_eq!(loom_rt_bytes_utf8(bytes), 1);
            let mut copied = loom_rt_bytes_text_copy(bytes);
            let copied_root = root(&mut copied);
            bytes_push(bytes, 255);
            assert_eq!(loom_rt_bytes_utf8(bytes), 0);
            assert_eq!(text_bytes(copied), "hé".as_bytes());
            let joined = loom_rt_text_concat(copied, copied);
            assert_eq!(text_bytes(joined), "héhé".as_bytes());
            assert_eq!(loom_rt_text_equal(copied, copied), 1);
            drop(copied_root);
        }
        drop(checkpoint);
        loom_rt_collect();
        assert_eq!(live(), 0);
    }

    #[test]
    fn text_slices_copy_utf8_ranges_and_unicode_queries_validate_scalars() {
        let mut source = text("A界🙂Z");
        let checkpoint = root(&mut source);
        HEAP.with(|heap| heap.borrow_mut().threshold = 0);
        // SAFETY: source and slice stay rooted across all copying allocations.
        unsafe {
            let mut slice = loom_rt_text_slice(source, 1, 8);
            let slice_root = root(&mut slice);
            assert_eq!(text_bytes(slice), "界🙂".as_bytes());
            assert_ne!(slice, source);
            HEAP.with(|heap| heap.borrow_mut().threshold = 0);
            assert_eq!(text_bytes(loom_rt_text_slice(slice, 3, 7)), "🙂".as_bytes());
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
            let checkpoint = root(&mut executable);
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
        let checkpoint = root(&mut path);
        let mut bytes = loom_rt_bytes_new();
        let bytes_root = root(&mut bytes);
        // SAFETY: Text and Bytes headers remain rooted and descriptors are only
        // used synchronously. Negative descriptors cannot name another resource.
        unsafe {
            let fd = loom_rt_file_open(path);
            assert!(fd >= 0);
            assert_eq!(loom_rt_file_read(fd, bytes, 2), 2);
            assert_eq!(loom_rt_file_read(fd, bytes, 2), 2);
            assert_eq!(loom_rt_file_read(fd, bytes, 2), 0);
            assert_eq!(buffer_bytes(bytes), b"loom");
            assert_eq!(loom_rt_file_close(fd), 0);
            assert_eq!(loom_rt_file_read(-1, bytes, 1), -1);
            assert_eq!(loom_rt_file_close(-1), -1);

            let mut output = text("prefix:ok");
            let output_root = root(&mut output);
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
            assert_eq!(loom_rt_file_read(fd, bytes, 16), 2);
            assert_eq!(buffer_bytes(bytes), b"ok");
            assert_eq!(loom_rt_file_close(fd), 0);
            drop(output_root);

            bytes = loom_rt_bytes_new();
            for byte in [b'x', 0, 255, 128, b'\n'] {
                bytes_push(bytes, byte);
            }
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
