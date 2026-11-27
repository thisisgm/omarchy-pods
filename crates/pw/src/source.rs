//! One stream source node: its loop, context, core and stream, and the three callbacks the library runs.

mod properties;

use std::ffi::{CStr, c_char, c_void};
use std::io;
use std::os::fd::{AsRawFd, IntoRawFd, OwnedFd};
use std::path::Path;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Once};
use std::time::Duration;

use rustix::net::{self, AddressFamily, SocketAddrUnix, SocketFlags, SocketType};

use crate::{ffi, pod};
use properties::{N_PROPERTIES, Properties};

/// A local socket answers in milliseconds; two seconds is a stuck server.
pub const PW_OPEN_MS: u64 = 2000;

/// The name of the loop's thread.
const LOOP_NAME: &CStr = c"pods-pw";
/// `PW_ID_ANY`: no target, and with no autoconnect flag the node links to nothing by itself.
const NO_TARGET: u32 = u32::MAX;
/// The bytes of one mono float frame.
const STRIDE: u32 = 4;

#[derive(Debug, Clone, Copy)]
pub struct Spec<'a> {
    /// The server's socket; no name in the environment replaces it.
    pub remote: &'a Path,
    pub name: &'a str,
    pub description: &'a str,
    /// The node offers mono float at this rate.
    pub rate: u32,
    pub latency_ns: u64,
    /// `priority.session`.
    pub priority: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Streaming,
    Paused,
    Lost,
}

/// Runs on the real time thread, so it never allocates, locks or panics; returns the rate ratio for the server's converter.
pub trait Fill: Send {
    /// `capacity` is the negotiated buffer size in frames, including space the current request did not use.
    fn fill(&mut self, out: &mut [f32], capacity: usize) -> f64;
}

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    /// The socket is absent, refuses the connection or has a full backlog (`WouldBlock`), the server hung up, or nothing answered for `PW_OPEN_MS` (`TimedOut`).
    #[error("no audio server: {0}")]
    NoServer(io::Error),
    /// The server answered and ended the stream in error before the node existed.
    #[error("the audio server refused the node: {0}")]
    Refused(String),
    /// Before any contact with a server: a call into the library failed, or a text for it holds a NUL and the call never ran (`InvalidInput`, with the property's key).
    #[error("{call}: {error}")]
    Library {
        call: &'static str,
        error: io::Error,
    },
}

impl OpenError {
    /// What a call that returned null left in `errno`.
    fn null(call: &'static str) -> Self {
        Self::Library {
            call,
            error: io::Error::last_os_error(),
        }
    }

    /// What a call that returned a negative errno said.
    fn negative(call: &'static str, result: i32) -> Self {
        Self::Library {
            call,
            error: io::Error::from_raw_os_error(result.saturating_neg()),
        }
    }
}

/// The fields drop in this order: the stream, the core and the context under the loop's lock, then the loop stops.
#[derive(Debug)]
pub struct Source {
    unsteered: Arc<AtomicU64>,
    stream: Stream,
    _core: Core,
    _context: Context,
    thread_loop: ThreadLoop,
}

#[expect(
    unsafe_code,
    reason = "a source is opened on one thread and dropped on another"
)]
// SAFETY: after `open` the pointers are used under the loop's lock, which lets any thread use them.
unsafe impl Send for Source {}

impl Source {
    /// Returns when the node exists on the server, or fails, within `PW_OPEN_MS`; `events` runs on the loop's thread and never drops the source.
    pub fn open(
        spec: &Spec<'_>,
        fill: Box<dyn Fill>,
        events: Box<dyn FnMut(Event) + Send>,
    ) -> Result<Self, OpenError> {
        let properties = Properties::new(spec)?;
        let socket = connect(spec.remote).map_err(OpenError::NoServer)?;
        let unsteered = Arc::new(AtomicU64::new(0));
        let params = [
            Pod::new(&pod::format(spec.rate)),
            Pod::new(&pod::process_latency(spec.latency_ns)),
        ];
        let (opened, verdict) = mpsc::sync_channel(1);
        let thread_loop = ThreadLoop::start()?;
        let source = {
            let _lock = Locked::take(thread_loop.0);
            let context = Context::new(&thread_loop)?;
            let core = Core::connect(&context, socket)?;
            let watch = Watch {
                phase: Phase::Opening(opened),
                events,
            };
            let steering = Steering {
                area: AtomicPtr::new(ptr::null_mut()),
                unsteered: Arc::clone(&unsteered),
            };
            let stream = Stream::connect(&core, &properties, &params, fill, steering, watch)?;
            Self {
                unsteered,
                stream,
                _core: core,
                _context: context,
                thread_loop,
            }
        };
        // The sender lives in the callbacks until it has sent, so the only error here is the bound.
        match verdict.recv_timeout(Duration::from_millis(PW_OPEN_MS)) {
            Ok(Ok(())) => Ok(source),
            Ok(Err(error)) => Err(error),
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
                Err(OpenError::NoServer(io::ErrorKind::TimedOut.into()))
            }
        }
    }

    /// The cycles in which the server's converter gave no area to steer through, so the ratio `Fill` returned went nowhere.
    #[must_use]
    pub fn unsteered(&self) -> u64 {
        self.unsteered.load(Ordering::Relaxed)
    }
}

/// Connects without blocking, as the stock client does, so a server with a full backlog fails at once and never holds `open`.
fn connect(remote: &Path) -> io::Result<OwnedFd> {
    let flags = SocketFlags::CLOEXEC | SocketFlags::NONBLOCK;
    let socket = net::socket_with(AddressFamily::UNIX, SocketType::STREAM, flags, None)?;
    net::connect(&socket, &SocketAddrUnix::new(remote)?)?;
    Ok(socket)
}

/// A pod at the alignment the library reads it with.
struct Pod(Vec<u64>);

impl Pod {
    #[expect(
        clippy::host_endian_bytes,
        reason = "a byte for byte copy into aligned words"
    )]
    fn new(bytes: &[u8]) -> Self {
        let words = bytes.as_chunks::<8>().0;
        Self(words.iter().map(|word| u64::from_ne_bytes(*word)).collect())
    }

    fn as_ptr(&self) -> *const ffi::spa_pod {
        self.0.as_ptr().cast()
    }
}

#[derive(Debug)]
struct ThreadLoop(NonNull<ffi::pw_thread_loop>);

impl ThreadLoop {
    #[expect(
        unsafe_code,
        reason = "starts the library and the thread that runs its loop"
    )]
    fn start() -> Result<Self, OpenError> {
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            // SAFETY: `pw_init` takes null for both arguments, and `Once` runs it one time for the process.
            unsafe { ffi::pw_init(ptr::null_mut(), ptr::null_mut()) };
        });
        // SAFETY: the name is a C string and the properties may be null.
        let raw = unsafe { ffi::pw_thread_loop_new(LOOP_NAME.as_ptr(), ptr::null()) };
        let thread_loop =
            Self(NonNull::new(raw).ok_or_else(|| OpenError::null("pw_thread_loop_new"))?);
        // SAFETY: the loop is live and not started yet.
        let started = unsafe { ffi::pw_thread_loop_start(thread_loop.0.as_ptr()) };
        if started < 0 {
            return Err(OpenError::negative("pw_thread_loop_start", started));
        }
        Ok(thread_loop)
    }
}

impl Drop for ThreadLoop {
    #[expect(unsafe_code, reason = "stops the loop's thread and frees the loop")]
    fn drop(&mut self) {
        // SAFETY: the loop is live, this is not its thread and the lock is not held; a loop that never started is left as it is.
        unsafe { ffi::pw_thread_loop_stop(self.0.as_ptr()) };
        // SAFETY: the thread has ended, and every object made on the loop was dropped before it.
        unsafe { ffi::pw_thread_loop_destroy(self.0.as_ptr()) };
    }
}

/// The loop's lock, which every call on the loop's objects from another thread holds; the loop outlives the guard.
struct Locked(NonNull<ffi::pw_thread_loop>);

impl Locked {
    #[expect(unsafe_code, reason = "takes the loop's lock")]
    fn take(thread_loop: NonNull<ffi::pw_thread_loop>) -> Self {
        // SAFETY: the loop is live, and its lock is recursive.
        unsafe { ffi::pw_thread_loop_lock(thread_loop.as_ptr()) };
        Self(thread_loop)
    }
}

impl Drop for Locked {
    #[expect(unsafe_code, reason = "gives the loop's lock back")]
    fn drop(&mut self) {
        // SAFETY: this thread took the lock when it made the guard.
        unsafe { ffi::pw_thread_loop_unlock(self.0.as_ptr()) };
    }
}

#[derive(Debug)]
struct Context {
    raw: NonNull<ffi::pw_context>,
    thread_loop: NonNull<ffi::pw_thread_loop>,
}

impl Context {
    #[expect(unsafe_code, reason = "makes the context on the loop")]
    fn new(thread_loop: &ThreadLoop) -> Result<Self, OpenError> {
        // SAFETY: the loop is live.
        let main_loop = unsafe { ffi::pw_thread_loop_get_loop(thread_loop.0.as_ptr()) };
        // SAFETY: the caller holds the loop's lock, the loop outlives the context, and null properties ask for the defaults.
        let raw = unsafe { ffi::pw_context_new(main_loop, ptr::null_mut(), 0) };
        Ok(Self {
            raw: NonNull::new(raw).ok_or_else(|| OpenError::null("pw_context_new"))?,
            thread_loop: thread_loop.0,
        })
    }
}

impl Drop for Context {
    #[expect(unsafe_code, reason = "frees the context under the lock of its loop")]
    fn drop(&mut self) {
        let _lock = Locked::take(self.thread_loop);
        // SAFETY: the context is live, the lock is held, and the stream and the core were dropped before it.
        unsafe { ffi::pw_context_destroy(self.raw.as_ptr()) };
    }
}

#[derive(Debug)]
struct Core {
    raw: NonNull<ffi::pw_core>,
    thread_loop: NonNull<ffi::pw_thread_loop>,
}

impl Core {
    /// Speaks the protocol on `socket`, so no name in the environment or the properties chooses the server.
    #[expect(unsafe_code, reason = "hands the connected socket to the library")]
    fn connect(context: &Context, socket: OwnedFd) -> Result<Self, OpenError> {
        let fd = socket.as_raw_fd();
        // SAFETY: the caller holds the loop's lock, the context is live and the descriptor is an open socket; null properties ask for the defaults.
        let raw =
            unsafe { ffi::pw_context_connect_fd(context.raw.as_ptr(), fd, ptr::null_mut(), 0) };
        // A failed call leaves the socket to this function, which closes it; a core closes it when it disconnects.
        let raw = NonNull::new(raw).ok_or_else(|| OpenError::null("pw_context_connect_fd"))?;
        let _closed_by_the_core = socket.into_raw_fd();
        Ok(Self {
            raw,
            thread_loop: context.thread_loop,
        })
    }
}

impl Drop for Core {
    #[expect(
        unsafe_code,
        reason = "disconnects the core under the lock of its loop"
    )]
    fn drop(&mut self) {
        let _lock = Locked::take(self.thread_loop);
        // SAFETY: the core is live, the lock is held, and the stream was dropped before it.
        unsafe { ffi::pw_core_disconnect(self.raw.as_ptr()) };
    }
}

#[derive(Debug)]
struct Stream {
    raw: NonNull<ffi::pw_stream>,
    callbacks: NonNull<Callbacks>,
    thread_loop: NonNull<ffi::pw_thread_loop>,
}

impl Stream {
    #[expect(
        unsafe_code,
        reason = "makes the stream, gives it the callbacks and connects it"
    )]
    fn connect(
        core: &Core,
        properties: &Properties,
        params: &[Pod; 2],
        fill: Box<dyn Fill>,
        steering: Steering,
        watch: Watch,
    ) -> Result<Self, OpenError> {
        let [(_, name), ..] = &properties.0;
        let items = properties
            .0
            .each_ref()
            .map(|(key, value)| ffi::spa_dict_item {
                key: key.as_ptr(),
                value: value.as_ptr(),
            });
        let dict = ffi::spa_dict {
            flags: 0,
            n_items: N_PROPERTIES,
            items: items.as_ptr(),
        };
        // SAFETY: the dictionary and its items are live for the call, which copies every string.
        let properties = unsafe { ffi::pw_properties_new_dict(&raw const dict) };
        if properties.is_null() {
            return Err(OpenError::null("pw_properties_new_dict"));
        }
        // SAFETY: the caller holds the loop's lock, the core is live, the name is a C string, and the stream takes the properties.
        let raw = unsafe { ffi::pw_stream_new(core.raw.as_ptr(), name.as_ptr(), properties) };
        let raw = NonNull::new(raw).ok_or_else(|| OpenError::null("pw_stream_new"))?;
        let callbacks = NonNull::from(Box::leak(Box::new(Callbacks {
            hook: ffi::spa_hook {
                link: ffi::spa_list {
                    next: ptr::null_mut(),
                    prev: ptr::null_mut(),
                },
                cb: ffi::spa_callbacks {
                    funcs: ptr::null(),
                    data: ptr::null_mut(),
                },
                removed: None,
                priv_: ptr::null_mut(),
            },
            process: Process {
                stream: raw.as_ptr(),
                fill,
            },
            steering,
            watch,
        })));
        let stream = Self {
            raw,
            callbacks,
            thread_loop: core.thread_loop,
        };
        // SAFETY: the callbacks were leaked above, and nothing else reads the hook yet.
        let hook = unsafe { &raw mut (*callbacks.as_ptr()).hook };
        let data = callbacks.as_ptr().cast();
        // SAFETY: the lock is held; the hook and the callbacks live until the stream is destroyed, and the events are a static.
        unsafe { ffi::pw_stream_add_listener(raw.as_ptr(), hook, &raw const EVENTS, data) };
        let mut params = params.each_ref().map(Pod::as_ptr);
        let flags = ffi::PW_STREAM_FLAG_MAP_BUFFERS | ffi::PW_STREAM_FLAG_RT_PROCESS;
        // SAFETY: the lock is held, and the two pods are whole, aligned and live for the call, which copies them.
        let connected = unsafe {
            ffi::pw_stream_connect(
                raw.as_ptr(),
                ffi::SPA_DIRECTION_OUTPUT,
                NO_TARGET,
                flags,
                params.as_mut_ptr(),
                2,
            )
        };
        if connected < 0 {
            return Err(OpenError::negative("pw_stream_connect", connected));
        }
        Ok(stream)
    }
}

impl Drop for Stream {
    #[expect(
        unsafe_code,
        reason = "ends the stream under the lock of its loop, then frees its callbacks"
    )]
    fn drop(&mut self) {
        let _lock = Locked::take(self.thread_loop);
        // SAFETY: the lock keeps the loop's thread, the only other user of the watch, out; the disconnect below then reports nothing.
        unsafe { (*self.callbacks.as_ptr()).watch.phase = Phase::Over };
        // SAFETY: the stream is live and the lock is held.
        unsafe { ffi::pw_stream_disconnect(self.raw.as_ptr()) };
        // SAFETY: as above; this takes the node off the data thread and removes the listener, so no callback runs after it.
        unsafe { ffi::pw_stream_destroy(self.raw.as_ptr()) };
        // SAFETY: `connect` leaked this box, and the library holds no pointer to it any more.
        drop(unsafe { Box::from_raw(self.callbacks.as_ptr()) });
    }
}

/// What the library calls back with: each field belongs to the thread named on it.
struct Callbacks {
    /// The library's, from the listener's start to the stream's end.
    hook: ffi::spa_hook,
    /// The data thread's.
    process: Process,
    /// Set by the loop's thread and used by the data thread.
    steering: Steering,
    /// The loop's thread's.
    watch: Watch,
}

struct Process {
    stream: *mut ffi::pw_stream,
    fill: Box<dyn Fill>,
}

/// The converter's rate match area, absent until the server gives one and after it takes it back.
struct Steering {
    area: AtomicPtr<ffi::spa_io_rate_match>,
    unsteered: Arc<AtomicU64>,
}

impl Steering {
    /// Keeps `area` when it holds a whole rate match, and nothing otherwise. The caller vouches that `area` is null or lives for as long as `steer` can run.
    #[expect(unsafe_code, reason = "takes the pointer `steer` writes through")]
    unsafe fn set(&self, area: *mut c_void, size: u32) {
        let whole =
            usize::try_from(size).is_ok_and(|size| size >= size_of::<ffi::spa_io_rate_match>());
        let area = if whole { area.cast() } else { ptr::null_mut() };
        self.area.store(area, Ordering::Release);
    }

    /// Asks the converter for `ratio`; with no area it writes nothing and counts the cycle.
    #[expect(unsafe_code, reason = "writes the area the converter reads")]
    fn steer(&self, ratio: f64) {
        let Some(area) = NonNull::new(self.area.load(Ordering::Acquire)) else {
            self.unsteered.fetch_add(1, Ordering::Relaxed);
            return;
        };
        // SAFETY: `set` took an area that outlives this call even when the loop's thread withdraws it meanwhile; the converter reads these two fields once a cycle.
        unsafe { (*area.as_ptr()).rate = ratio.recip() };
        // SAFETY: as above.
        unsafe { (*area.as_ptr()).flags |= ffi::SPA_IO_RATE_MATCH_FLAG_ACTIVE };
    }
}

struct Watch {
    phase: Phase,
    events: Box<dyn FnMut(Event) + Send>,
}

enum Phase {
    /// `open` waits for the node, or for why there is none.
    Opening(SyncSender<Result<(), OpenError>>),
    Open,
    /// The stream is lost or being dropped, and nothing more is reported.
    Over,
}

/// Version 0: the three callbacks set here are in every version of the struct.
static EVENTS: ffi::pw_stream_events = ffi::pw_stream_events {
    version: 0,
    destroy: None,
    state_changed: Some(on_state),
    control_info: None,
    io_changed: Some(on_io),
    param_changed: None,
    add_buffer: None,
    remove_buffer: None,
    process: Some(on_process),
    drained: None,
    command: None,
    trigger_done: None,
};

impl Watch {
    fn changed(&mut self, state: ffi::pw_stream_state, reason: String) {
        let event = match state {
            ffi::PW_STREAM_STATE_PAUSED => Event::Paused,
            ffi::PW_STREAM_STATE_STREAMING => Event::Streaming,
            ffi::PW_STREAM_STATE_ERROR | ffi::PW_STREAM_STATE_UNCONNECTED => Event::Lost,
            _ => return,
        };
        let lost = event == Event::Lost;
        let next = if lost { Phase::Over } else { Phase::Open };
        match std::mem::replace(&mut self.phase, next) {
            Phase::Opening(opened) => {
                let verdict = match state {
                    _ if !lost => Ok(()),
                    ffi::PW_STREAM_STATE_ERROR => Err(OpenError::Refused(reason)),
                    _ => Err(OpenError::NoServer(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        reason,
                    ))),
                };
                // The channel holds one verdict and this is the only one sent, so the send finds room.
                drop(opened.try_send(verdict));
            }
            Phase::Open => (self.events)(event),
            Phase::Over => self.phase = Phase::Over,
        }
    }
}

#[expect(
    unsafe_code,
    reason = "the library's state callback, on the loop's thread"
)]
extern "C" fn on_state(
    data: *mut c_void,
    _old: ffi::pw_stream_state,
    state: ffi::pw_stream_state,
    error: *const c_char,
) {
    let reason = if error.is_null() {
        String::from("the connection ended")
    } else {
        // SAFETY: a non-null error is the library's C string, live for this call.
        let error = unsafe { CStr::from_ptr(error) };
        error.to_string_lossy().into_owned()
    };
    // SAFETY: `data` is the callbacks given with the listener, and the watch is used only on this thread, or under its lock.
    let watch = unsafe { &mut (*data.cast::<Callbacks>()).watch };
    watch.changed(state, reason);
}

#[expect(
    unsafe_code,
    reason = "the library's io callback, on the loop's thread"
)]
extern "C" fn on_io(data: *mut c_void, id: u32, area: *mut c_void, size: u32) {
    if id != ffi::SPA_IO_RateMatch {
        return;
    }
    // SAFETY: `data` is the callbacks given with the listener, and the field is shared through atomics.
    let steering = unsafe { &(*data.cast::<Callbacks>()).steering };
    // SAFETY: the library gives null or the area inside the stream's converter, which is freed with the node, after its last process call.
    unsafe { steering.set(area, size) };
}

/// Fills one buffer; allocates nothing, takes no lock and writes no log.
#[expect(
    unsafe_code,
    reason = "the library's process callback, on the data thread"
)]
extern "C" fn on_process(data: *mut c_void) {
    let callbacks = data.cast::<Callbacks>();
    // SAFETY: `data` is the callbacks given with the listener, and only this thread uses the process state while the stream lives.
    let process = unsafe { &mut (*callbacks).process };
    // SAFETY: the stream is live while its callbacks run, and this is its data thread.
    let buffer = unsafe { ffi::pw_stream_dequeue_buffer(process.stream) };
    // SAFETY: a dequeued buffer is null or this callback's alone until it is queued again.
    let Some(buffer) = (unsafe { buffer.as_mut() }) else {
        return;
    };
    // SAFETY: the buffer came from the stream, with its data mapped by the library.
    if let Some(frames) = unsafe { frames(buffer) } {
        let ratio = process.fill.fill(frames.samples, frames.capacity);
        *frames.chunk = ffi::spa_chunk {
            offset: 0,
            size: frames.count.saturating_mul(STRIDE),
            stride: STRIDE.cast_signed(),
            flags: 0,
        };
        buffer.size = u64::from(frames.count);
        // SAFETY: as for the process state; the field is shared through atomics.
        let steering = unsafe { &(*callbacks).steering };
        steering.steer(ratio);
    }
    // SAFETY: the buffer was dequeued from this stream above; one the reader left alone goes back as it came, or the pool would run dry.
    unsafe { ffi::pw_stream_queue_buffer(process.stream, buffer) };
}

/// The part of a buffer `Fill` writes, its total capacity and the chunk that tells the server how much it wrote.
struct Frames<'a> {
    samples: &'a mut [f32],
    count: u32,
    capacity: usize,
    chunk: &'a mut ffi::spa_chunk,
}

/// The frames the converter asked for, or all the buffer holds when it asked for none; `None` for a buffer that is not whole mapped float frames. The caller vouches that each pointer in `buffer` is null or the library's.
#[expect(unsafe_code, reason = "reads the buffer the library handed over")]
unsafe fn frames(buffer: &ffi::pw_buffer) -> Option<Frames<'_>> {
    // SAFETY: the pointer is null or the library's buffer.
    let spa = unsafe { buffer.buffer.as_ref() }?;
    if spa.n_datas == 0 {
        return None;
    }
    // SAFETY: the pointer is null or the first of `n_datas` blocks, and there is at least one.
    let data = unsafe { spa.datas.as_ref() }?;
    // SAFETY: the pointer is null or the block's chunk, which only this callback writes while it holds the buffer.
    let chunk = unsafe { data.chunk.as_mut() }?;
    let samples = data.data.cast::<f32>();
    if samples.is_null() || !samples.is_aligned() || !data.maxsize.is_multiple_of(STRIDE) {
        return None;
    }
    let held = data.maxsize.checked_div(STRIDE)?;
    let asked = u32::try_from(buffer.requested).unwrap_or(held);
    let count = if asked == 0 { held } else { asked.min(held) };
    // SAFETY: `count` frames are at most `maxsize` mapped bytes, aligned for a float, and this callback's alone until the buffer is queued.
    let samples = unsafe { std::slice::from_raw_parts_mut(samples, usize::try_from(count).ok()?) };
    Some(Frames {
        samples,
        count,
        capacity: usize::try_from(held).ok()?,
        chunk,
    })
}

#[cfg(test)]
mod tests;
