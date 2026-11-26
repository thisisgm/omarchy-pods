//! Support test: every allocation the process callback's thread makes through the Rust allocator is counted while a recorder is linked; no user path yet.

mod support;

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod tests {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;
    use std::hint::black_box;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::Duration;

    use pods_pw::{Fill, Source};
    use pods_pwrig::tone::frames;
    use pods_pwrig::{Recorder, Server};

    use crate::support::{NAME, isolate, spec};

    /// How long the recorder records before the count is read.
    const STREAMED: Duration = Duration::from_secs(2);
    /// Recorded after the allocation made on purpose, so the callback that makes it has run.
    const AFTER: Duration = Duration::from_millis(500);
    /// A local server answers in milliseconds; this is a hung one.
    const WITHIN: Duration = Duration::from_secs(5);

    /// Allocations on the callback's thread, from the first call of `Fill` on.
    static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
    static CALLBACKS: AtomicU64 = AtomicU64::new(0);
    /// Makes the next callback allocate once, to show that the count sees its thread.
    static ALLOCATE_ONCE: AtomicBool = AtomicBool::new(false);

    thread_local! {
        static CALLBACK_THREAD: Cell<bool> = const { Cell::new(false) };
    }

    struct Counting;

    #[expect(unsafe_code, reason = "a global allocator is an unsafe trait")]
    // SAFETY: every call goes to the system allocator unchanged, and the count touches no memory of the allocation.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            // A thread that is ending has no flag left to read, and is not the callback's.
            if CALLBACK_THREAD.try_with(Cell::get).unwrap_or(false) {
                ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            }
            // SAFETY: the caller keeps the allocator's contract for `layout`.
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, block: *mut u8, layout: Layout) {
            // SAFETY: the caller keeps the allocator's contract for `block` and `layout`.
            unsafe { System.dealloc(block, layout) }
        }
    }

    #[global_allocator]
    static ALLOCATOR: Counting = Counting;

    struct Silence;

    impl Fill for Silence {
        fn fill(&mut self, out: &mut [f32], _capacity: usize) -> f64 {
            CALLBACK_THREAD.with(|flag| flag.set(true));
            if ALLOCATE_ONCE.swap(false, Ordering::Relaxed) {
                drop(black_box(Box::new(0_u8)));
            }
            out.fill(0.0);
            CALLBACKS.fetch_add(1, Ordering::Relaxed);
            1.0
        }
    }

    #[test]
    fn the_process_callback_never_allocates() {
        let dir = tempfile::tempdir().unwrap();
        isolate(dir.path());
        let server = Server::start(dir.path());
        let socket = server.socket();
        let _source = Source::open(&spec(&socket), Box::new(Silence), Box::new(|_| {})).unwrap();
        let recorder = Recorder::superwhisper(&server, NAME, &dir.path().join("capture.wav"));
        server.link(NAME, recorder.node());
        recorder.wait_frames(frames(STREAMED, recorder.rate()), WITHIN);
        let callbacks = CALLBACKS.load(Ordering::Relaxed);
        assert!(callbacks > 1, "{callbacks} callbacks in {STREAMED:?}");
        assert_eq!(
            ALLOCATIONS.load(Ordering::Relaxed),
            0,
            "allocations on the callback's thread across {callbacks} callbacks"
        );
        // A zero proves nothing unless the count sees that thread: one allocation made there on purpose is counted.
        ALLOCATE_ONCE.store(true, Ordering::Relaxed);
        recorder.wait_frames(frames(STREAMED + AFTER, recorder.rate()), WITHIN);
        assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 1);
    }
}
